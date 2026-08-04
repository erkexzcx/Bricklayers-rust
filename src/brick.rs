//! Brick layering.
//!
//! Inside every internal perimeter region the loops are numbered and every
//! other one is raised by half a layer height. Adjacent loops then bond across
//! a staggered seam instead of stacking their weak points on top of each other,
//! the same way courses of bricks are offset.
//!
//! One region covers an island's outer wall, the walls of every hole in it and
//! whatever fragments a thin wall broke into, so the numbering restarts at each
//! contour. Otherwise a contour that gained or lost a loop would invert the
//! stagger of every contour printed after it.
//!
//! External perimeters are never touched, so the visible surface is unchanged.

use std::io::{self, BufRead, Write};

use crate::feature::{Feature, is_layer_marker};
use crate::gcode::{Code, Extruder, Line, Lines, write_e};
use crate::scan::{BRICK_STAMP, FALLBACK_Z_FEEDRATE, Survey, is_a_height};

/// How far apart two loops may run and still count as neighbours in one wall,
/// in mm.
///
/// One loop of a wall is the last one offset inwards, so they run an extrusion
/// width apart. This is a generous ceiling on that: the widest bead a 1.2 mm
/// nozzle lays down. Measured over four real prints, neighbouring loops run
/// 0.4 to 1.5 mm apart and the next island is more than 3 mm away, with almost
/// nothing in between. Erring low only splits a wall, which costs the stagger;
/// erring high staggers loops that never touch.
const MAX_LOOP_GAP: f64 = 2.0;

/// Points sampled from a loop when testing it against the one before it.
///
/// Every point of an offset loop is a witness, so one would usually do. A
/// handful covers the loop that runs on past the end of the one it followed,
/// which is what a wall does wherever it widens.
const PROBES: usize = 16;

#[derive(Clone, Copy, Debug)]
pub struct Config {
    /// Layer height in mm. Detected from the file when `None`.
    pub layer_height: Option<f64>,
    /// Height of the first layer in mm, which slicers commonly print thicker
    /// than the rest. Falls back to the layer height when neither the file nor
    /// the slicer says.
    pub first_layer_height: Option<f64>,
    /// Extra extrusion for raised loops on middle layers.
    ///
    /// Volume alone says 1.0, and that is the default: a raised column stacks
    /// flush like any other, and the two ends of it are metered separately.
    /// Going above it compensates for a raised bead being laid against a step
    /// rather than a flat plane, where the nozzle cannot press the seam corner
    /// closed — a physical constant, not a derived quantity, which is why
    /// nothing here picks one for you. It is cheap when you do: raised loops
    /// carry roughly a third of a part's filament (30.5% on a 240-layer
    /// Benchy), so 1.05 adds about 1.5% to its mass where the same figure on a
    /// slicer's global flow would add 5%.
    pub extrusion_multiplier: f64,
    /// True when the slicer prints the external perimeter before the loops
    /// behind it, which decides which end of a wall the numbering starts from.
    /// Every mainstream slicer prints it last by default.
    pub external_perimeters_first: bool,
    /// Print a layer's unraised loops before its raised ones, so the nozzle
    /// changes height once per layer instead of once per loop.
    pub reorder_loops: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            layer_height: None,
            first_layer_height: None,
            extrusion_multiplier: 1.0,
            external_perimeters_first: false,
            reorder_loops: false,
        }
    }
}

/// What a rewrite did, for reporting.
#[derive(Clone, Copy, Debug)]
pub struct Stats {
    pub layer_height: f64,
    /// False when [`Config::layer_height`] was `None` and the file gave no hint.
    pub layer_height_detected: bool,
    pub layers: usize,
    pub loops: usize,
    pub raised: usize,
    /// Filament the output calls for, in mm of stock. Retractions are ignored.
    pub filament: f64,
    /// The part of `filament` laid down by raised loops.
    pub raised_filament: f64,
    /// The part of `filament` that [`Config::extrusion_multiplier`] added over
    /// a factor of 1.0.
    pub multiplier_filament: f64,
}

#[derive(Clone, Debug)]
pub struct Outcome {
    pub gcode: String,
    pub stats: Stats,
}

/// Rewrites a G-code stream, reading and writing a line at a time.
///
/// `survey` comes from an earlier pass over the same stream; see
/// [`Survey::read`].
pub fn stream<R: BufRead, W: Write>(
    reader: R,
    writer: W,
    config: &Config,
    survey: &Survey,
) -> io::Result<Stats> {
    let mut pass = Pass::new(writer, config, survey);
    let mut lines = Lines::new(reader);
    while let Some(raw) = lines.next_line()? {
        pass.feed(raw)?;
    }
    pass.flush()?;
    pass.out.flush()?;

    Ok(Stats {
        layer_height: pass.shift * 2.0,
        layer_height_detected: config.layer_height.is_some() || survey.layer_height_detected,
        layers: survey.layers,
        loops: pass.loops_seen,
        raised: pass.raised,
        filament: pass.filament,
        raised_filament: pass.raised_filament,
        multiplier_filament: pass.multiplier_filament,
    })
}

/// Rewrites G-code held in memory. Convenient for short inputs and tests; a
/// file goes through [`stream`] instead.
pub fn apply(source: &str, config: &Config) -> Outcome {
    let survey = Survey::of(source);
    let mut out = Vec::with_capacity(source.len() + source.len() / 8);
    let stats =
        stream(source.as_bytes(), &mut out, config, &survey).expect("writing to a Vec cannot fail");

    Outcome {
        gcode: String::from_utf8(out).expect("rewritten G-code is UTF-8"),
        stats,
    }
}

/// A buffered line, with the extrusion it asked for already resolved against
/// the input stream so that loops can be reordered safely. The text lives in
/// the pass's arena, which is why this is a span rather than a borrow.
#[derive(Clone, Copy)]
struct Buffered {
    start: usize,
    end: usize,
    /// Byte range of the `E` word's digits within the line, so rescaling it
    /// needs no second parse.
    e_span: Option<(usize, usize)>,
    /// The value the line was written with, which is what decides whether it
    /// has to be written again.
    e: Option<f64>,
    delta: Option<f64>,
    z: Option<f64>,
    f: Option<f64>,
    xy: Option<(f64, f64)>,
    extrudes: bool,
}

/// One perimeter loop, as index ranges into the buffer. `lead` covers the
/// travel that reaches the loop, `body` the extrusions themselves.
#[derive(Clone, Copy)]
struct Loop {
    lead: usize,
    body: usize,
    /// One past this loop's last buffered line, known only once the region is
    /// complete.
    end: usize,
    /// Which contour of the region this loop belongs to, so a contour that
    /// holds only one loop can be told apart from a wall that alternates.
    contour: usize,
    raised: bool,
    /// Extent of what the loop extrudes, as `[left, bottom, right, top]`, and
    /// how many points it lays down. Measured once, since grouping compares
    /// every loop with both the one before it and the one after.
    outline: Option<[f64; 4]>,
    points: usize,
}

struct Pass<'a, W: Write> {
    config: &'a Config,
    /// Layer each object starts at. A file that completes objects one at a
    /// time has several first and last layers, not just the file's own.
    object_starts: Vec<usize>,
    /// Layer each object's walls top out at, which is where solid infill takes
    /// over rather than where the file ends.
    object_tops: Vec<usize>,
    layer_markers: bool,
    shift: f64,
    /// Height of the first layer, which the raised loops of that layer have to
    /// span from the bed rather than from the layer below.
    first_layer_height: f64,
    out: W,
    extruder: Extruder,
    feature: Feature,
    layer: usize,
    started: bool,
    layer_z: f64,
    nozzle_z: Option<f64>,
    /// Rate for the Z moves this pass inserts.
    z_feedrate: f64,
    /// Feedrate the output stream is currently left in, since `F` is modal and
    /// an inserted Z move would otherwise hand its own rate to the next print.
    feedrate: Option<f64>,
    /// Text of the region being buffered. Cleared and refilled at each flush,
    /// so it only ever holds one perimeter region.
    arena: String,
    buffer: Vec<Buffered>,
    loops: Vec<Loop>,
    /// Replay order, reused between regions and only filled when the loops of
    /// one are actually being reordered.
    order: Vec<usize>,
    travelled: bool,
    loops_seen: usize,
    raised: usize,
    filament: f64,
    raised_filament: f64,
    multiplier_filament: f64,
}

impl<'a, W: Write> Pass<'a, W> {
    fn new(out: W, config: &'a Config, survey: &Survey) -> Self {
        let layer_height = config
            .layer_height
            .filter(is_a_height)
            .unwrap_or(survey.layer_height);
        Self {
            config,
            object_starts: survey.object_starts.clone(),
            object_tops: survey.object_tops.clone(),
            layer_markers: survey.layer_markers,
            shift: layer_height / 2.0,
            first_layer_height: config
                .first_layer_height
                .or(survey.first_layer_height)
                .filter(is_a_height)
                .unwrap_or(layer_height),
            out,
            extruder: Extruder::new(),
            feature: Feature::Other,
            layer: 0,
            started: false,
            layer_z: 0.0,
            nozzle_z: None,
            z_feedrate: survey.z_feedrate.unwrap_or(FALLBACK_Z_FEEDRATE),
            feedrate: None,
            arena: String::new(),
            buffer: Vec::new(),
            loops: Vec::new(),
            order: Vec::new(),
            travelled: false,
            loops_seen: 0,
            raised: 0,
            filament: 0.0,
            raised_filament: 0.0,
            multiplier_filament: 0.0,
        }
    }

    fn feed(&mut self, raw: &str) -> io::Result<()> {
        let line = Line::parse(raw);
        if let Some(marker) = line.marker() {
            if is_layer_marker(marker) {
                self.flush()?;
                // Slicers re-declare the region after a layer change, and some
                // open the next wall with a stray segment before they do.
                // Carrying the old region across would buffer that segment as
                // a perimeter loop of its own.
                self.feature = Feature::Other;
                self.layer += usize::from(std::mem::replace(&mut self.started, true));
                return self.push(raw);
            }
            if let Some(feature) = Feature::from_marker(marker) {
                self.flush()?;
                self.feature = feature;
                return self.push(raw);
            }
        }

        match line.code {
            Code::AbsoluteE | Code::RelativeE => self.extruder.set_mode(line.code),
            Code::SetPosition => {
                // A `G92` redefines the extruder origin, and the buffered
                // region's moves have not been metered out yet: emitting them
                // after the reset would measure their absolute positions from
                // the wrong zero. Cura resets the origin periodically to keep
                // `E` from growing without bound, so this is on the path the
                // absolute-extrusion support exists for.
                self.flush()?;
                if let Some(e) = line.e {
                    self.extruder.set_position(e);
                }
            }
            _ => {}
        }

        // Klipper and Orca without a Z-hop put a layer's Z on the travel that
        // reaches the first loop, which lands inside the buffered region.
        if let Some(z) = line.z.filter(|_| line.is_move()) {
            if !self.layer_markers && z > self.layer_z {
                self.layer += usize::from(std::mem::replace(&mut self.started, true));
            }
            self.layer_z = z;
        }

        if self.feature == Feature::InternalPerimeter {
            self.buffer(raw, line);
            return Ok(());
        }

        if let Some(z) = line.z.filter(|_| line.is_move()) {
            self.nozzle_z = Some(z);
        }
        if let Some(rate) = line.f {
            self.feedrate = Some(rate);
        }
        self.emit(raw, line, 1.0)
    }

    fn buffer(&mut self, raw: &str, line: Line<'_>) {
        // A `G92` carries an `E` that sets the origin rather than asking for
        // filament, and `set_position` has already dealt with it.
        let delta = line
            .e
            .filter(|_| line.draws())
            .map(|e| self.extruder.observe(e));
        let xy = line.xy();
        // Arc fitting turns a run of short segments into one G2/G3. Leaving
        // those out of a loop would strand its opening arcs in the travel that
        // reaches it, to be printed before the nozzle rises.
        let extrudes = line.draws() && xy.is_some() && delta.is_some_and(|d| d > 0.0);

        let index = self.buffer.len();
        let start = self.arena.len();
        self.arena.push_str(raw);
        self.buffer.push(Buffered {
            start,
            end: self.arena.len(),
            e_span: line.e_span(),
            e: line.e,
            delta,
            z: line.z.filter(|_| line.is_move()),
            f: line.f,
            xy,
            extrudes,
        });

        if extrudes {
            if self.loops.is_empty() || self.travelled {
                self.open_loop(index);
            }
        } else if line.is_xy_move() {
            self.travelled = true;
        }
    }

    fn open_loop(&mut self, body: usize) {
        // Pull the travel that reaches this loop in with it, so reordering
        // loops keeps them reachable.
        let floor = self.loops.last().map_or(0, |previous| previous.body + 1);
        let mut lead = body;
        while lead > floor && !self.buffer[lead - 1].extrudes {
            lead -= 1;
        }
        self.loops.push(Loop {
            lead,
            body,
            end: 0,
            contour: 0,
            raised: false,
            outline: None,
            points: 0,
        });
        self.travelled = false;
    }

    fn flush(&mut self) -> io::Result<()> {
        if self.buffer.is_empty() {
            return Ok(());
        }

        self.assign_contours();
        self.number_loops();

        let head = self.loops.first().map_or(self.buffer.len(), |l| l.lead);
        for index in 0..head {
            self.replay(index, 1.0)?;
        }

        if self.config.reorder_loops {
            let Self { order, loops, .. } = self;
            order.clear();
            order.extend(0..loops.len());
            order.sort_by_key(|&index| loops[index].raised);
        }

        let mut last_raised = false;
        for position in 0..self.loops.len() {
            let index = if self.config.reorder_loops {
                self.order[position]
            } else {
                position
            };
            let current = self.loops[index];
            let end = current.end;

            for at in current.lead..current.body {
                self.replay(at, 1.0)?;
            }
            // The top layer caps the wall, so it stays on the plane however the
            // parity fell: raising it would stand a bead half a layer proud of
            // the surface beside it, over a gap the layer below already half
            // filled. `extrusion_factor` meters that half gap.
            let raise = current.raised && !self.capping();
            // After the lead, so a slicer's own Z-hop restore cannot undo it.
            let target = if raise {
                self.layer_z + self.shift
            } else {
                self.layer_z
            };
            self.move_z(target, raise)?;
            let factor = self.extrusion_factor(current.raised);
            if current.raised {
                self.meter(current.body, end, factor);
            }
            for at in current.body..end {
                self.replay(at, factor)?;
            }

            last_raised = raise;
            self.loops_seen += 1;
            self.raised += usize::from(raise);
        }

        if last_raised {
            self.move_z(self.layer_z, false)?;
        }

        self.arena.clear();
        self.buffer.clear();
        self.loops.clear();
        self.travelled = false;
        Ok(())
    }

    /// Groups the region's loops into contours and numbers them.
    ///
    /// Two loops belong to the same wall when they run beside each other, an
    /// extrusion width apart, which is what a slicer emits: each loop is the
    /// last one offset inwards. Anything else — a hole, another island, one of
    /// the fragments a thin wall breaks into — starts a contour of its own, so
    /// the alternation is always measured from the outermost loop of the wall
    /// it belongs to.
    ///
    /// Retraction was the obvious signal here and is the wrong one: slicers
    /// retract between neighbouring loops of one wall, and cross to another
    /// island without retracting whenever the travel is short. The distance
    /// between one loop's end and the next one's start is no better, since the
    /// seam can sit anywhere on the loop.
    fn assign_contours(&mut self) {
        for index in 0..self.loops.len() {
            let end = self
                .loops
                .get(index + 1)
                .map_or(self.buffer.len(), |next| next.lead);
            let (outline, points) = self.measure(self.loops[index].body, end);
            let current = &mut self.loops[index];
            current.end = end;
            current.outline = outline;
            current.points = points;
        }

        let mut contour = 0;
        for index in 0..self.loops.len() {
            if index == 0 || !self.adjacent(index - 1, index) {
                contour += 1;
            }
            self.loops[index].contour = contour;
        }
    }

    /// Numbers each contour's loops outwards from the visible wall.
    ///
    /// Which loop is number one decides which loops are raised, so it has to be
    /// a loop that stays put. A wall gains and loses loops as it thickens, and
    /// always at the hidden end: number from there and every loop shifts one
    /// place the moment the count changes, inverting the stagger. One column
    /// then gains half a layer of doubled material and its neighbour opens a
    /// half-layer void, which is weaker than the plain seam this exists to
    /// remove. On a Benchy hull that happens every third layer or so.
    ///
    /// The loop against the external perimeter is the one that stays, and
    /// slicers print it either first or last depending on the wall order.
    ///
    /// A contour holding one loop is raised too. It has no internal loop to
    /// alternate with, but an internal perimeter exists only because the
    /// slicer inset it from an external one, so the wall that shows always
    /// runs beside it — and where a solid wall is about three beads thick, on
    /// both sides of it. Measured on a 240-layer Benchy: lone contours carry
    /// 8.7% of the internal perimeter at a median of 13 mm of path each, so
    /// they are walls rather than the slivers a lone contour sounds like.
    fn number_loops(&mut self) {
        // Loops arrive in print order, so one contour's loops are contiguous.
        let mut start = 0;
        while start < self.loops.len() {
            let contour = self.loops[start].contour;
            let mut end = start + 1;
            while end < self.loops.len() && self.loops[end].contour == contour {
                end += 1;
            }
            let loops = end - start;
            for offset in 0..loops {
                let phase = if self.config.external_perimeters_first {
                    offset
                } else {
                    loops - 1 - offset
                };
                self.loops[start + offset].raised = phase.is_multiple_of(2);
            }
            start = end;
        }
    }

    /// True when any part of the two loops runs within [`MAX_LOOP_GAP`] of the
    /// other.
    fn adjacent(&self, previous: usize, current: usize) -> bool {
        let (previous, current) = (self.loops[previous], self.loops[current]);
        // Cheap rejection first: most pairs are a travel apart, and comparing
        // their extents settles that without touching either path.
        let (Some(before), Some(now)) = (previous.outline, current.outline) else {
            return false;
        };
        let apart = (before[0] - now[2])
            .max(now[0] - before[2])
            .max(before[1] - now[3])
            .max(now[1] - before[3]);
        if apart > MAX_LOOP_GAP {
            return false;
        }

        let stride = current.points.div_ceil(PROBES).max(1);
        let limit = MAX_LOOP_GAP * MAX_LOOP_GAP;
        self.points(current.body, current.end)
            .step_by(stride)
            .any(|(x, y)| {
                self.points(previous.body, previous.end).any(|(px, py)| {
                    let (dx, dy) = (px - x, py - y);
                    dx * dx + dy * dy <= limit
                })
            })
    }

    /// The points a loop lays down, in print order.
    fn points(&self, from: usize, to: usize) -> impl Iterator<Item = (f64, f64)> + '_ {
        self.buffer[from..to]
            .iter()
            .filter(|buffered| buffered.extrudes)
            .filter_map(|buffered| buffered.xy)
    }

    /// The extent of what a loop extrudes, as `[left, bottom, right, top]`,
    /// and how many points it lays down.
    fn measure(&self, from: usize, to: usize) -> (Option<[f64; 4]>, usize) {
        let mut outline: Option<[f64; 4]> = None;
        let mut points = 0;
        for (x, y) in self.points(from, to) {
            outline = Some(outline.map_or([x, y, x, y], |box_: [f64; 4]| {
                [
                    box_[0].min(x),
                    box_[1].min(y),
                    box_[2].max(x),
                    box_[3].max(y),
                ]
            }));
            points += 1;
        }
        (outline, points)
    }

    fn replay(&mut self, index: usize, factor: f64) -> io::Result<()> {
        let buffered = self.buffer[index];
        if let Some(z) = buffered.z {
            self.nozzle_z = Some(z);
        }
        if let Some(rate) = buffered.f {
            self.feedrate = Some(rate);
        }
        if let Some(delta) = buffered.delta.filter(|delta| *delta > 0.0) {
            self.filament += delta * factor;
        }

        let Self {
            arena,
            out,
            extruder,
            ..
        } = self;
        let raw = &arena[buffered.start..buffered.end];
        let Some(delta) = buffered.delta else {
            return write_line(out, raw);
        };

        let value = extruder.advance(delta * factor);
        // Not `Extruder::is_drifting`: the input position has already run to
        // the end of the buffered region, so it says nothing about this line.
        // Whether the line has to be rewritten is whether the value it should
        // now carry differs from the one it was written with.
        if buffered.e == Some(value) {
            return write_line(out, raw);
        }
        write_e(out, raw, buffered.e_span, value)?;
        out.write_all(b"\n")
    }

    fn emit(&mut self, raw: &str, line: Line<'_>, factor: f64) -> io::Result<()> {
        let Some(e) = line.e.filter(|_| line.draws()) else {
            return self.push(raw);
        };
        let delta = self.extruder.observe(e);
        if delta > 0.0 {
            self.filament += delta * factor;
        }
        let value = self.extruder.advance(delta * factor);
        if value == e {
            return self.push(raw);
        }
        line.write_e(&mut self.out, value)?;
        self.out.write_all(b"\n")
    }

    /// Raised loops need more filament on the first layer of an object, where
    /// their bead has to reach all the way down to the bed, and less on its
    /// last, where the raised loop below has already filled all but half of
    /// the gap.
    fn extrusion_factor(&self, raised: bool) -> f64 {
        if !raised {
            1.0
        } else if self.opening() {
            // The slicer metered this bead for the first layer's height; raised,
            // it spans that plus the shift. Assuming the two heights are equal
            // is what makes a thick first layer come out starved.
            (self.first_layer_height + self.shift) / self.first_layer_height
        } else if self.capping() {
            0.5
        } else {
            self.config.extrusion_multiplier
        }
    }

    /// True on a layer laid straight onto the bed, which is the first of the
    /// print and the first of every later object a sequential file builds.
    fn opening(&self) -> bool {
        self.object_starts.contains(&self.layer)
    }

    /// True on a layer that tops an object's walls, which has nothing above it
    /// to interlock with.
    ///
    /// Not the object's last layer. A part is closed by solid infill laid over
    /// its walls, so the two differ on every real file measured, and testing
    /// the layer count left the topmost wall raised half a layer proud of the
    /// surface printed over it.
    fn capping(&self) -> bool {
        self.object_tops.contains(&self.layer)
    }

    /// True away from the two ends of a raised column, which are the layers
    /// where a derived factor rather than the multiplier sets the flow.
    fn middle_layer(&self) -> bool {
        !self.opening() && !self.capping()
    }

    /// Books a raised loop's filament, and the share of it the multiplier
    /// added, so `--verbose` can price the setting against the whole part.
    fn meter(&mut self, body: usize, end: usize, factor: f64) {
        let stock: f64 = self.buffer[body..end]
            .iter()
            .filter_map(|buffered| buffered.delta)
            .filter(|delta| *delta > 0.0)
            .sum();
        self.raised_filament += stock * factor;
        if self.middle_layer() {
            self.multiplier_filament += stock * (factor - 1.0);
        }
    }

    fn move_z(&mut self, z: f64, raised: bool) -> io::Result<()> {
        if self.nozzle_z.is_some_and(|current| current == z) {
            return Ok(());
        }
        self.nozzle_z = Some(z);
        let note = if raised { "raised" } else { "reset" };
        let rate = self.z_feedrate;
        writeln!(self.out, "G1 Z{z:.3} F{rate:.0} ; {BRICK_STAMP}{note}")?;
        match self.feedrate {
            Some(previous) if previous != rate => {
                writeln!(self.out, "G1 F{previous:.0} ; {BRICK_STAMP}resume")
            }
            _ => Ok(()),
        }
    }

    fn push(&mut self, line: &str) -> io::Result<()> {
        write_line(&mut self.out, line)
    }
}

fn write_line<W: Write>(out: &mut W, line: &str) -> io::Result<()> {
    out.write_all(line.as_bytes())?;
    out.write_all(b"\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layer(z: f64) -> String {
        format!(";LAYER_CHANGE\nG1 Z{z:.2} F600\n")
    }

    fn relative(body: &str) -> String {
        format!("; layer_height = 0.2\nM83\n{body}")
    }

    /// A file whose middle layer carries `body`, so neither the first-layer nor
    /// the last-layer extrusion factor applies.
    ///
    /// A wall carries on above it, as it does in a real file. Without that the
    /// body would itself be the last layer holding a wall, which is what caps
    /// one, and every test built on this would measure a capped layer. Its
    /// loop is left untagged so it stays out of [`loop_states`].
    fn middle_layer(body: &str) -> String {
        relative(&format!(
            "{}{}{body}{}{}",
            layer(0.2),
            layer(0.4),
            layer(0.6),
            ";TYPE:Perimeter\n\
             G1 X0 Y0 F9000\n\
             G1 X10 Y0 E0.5\n\
             G1 X10 Y10 E0.5\n\
             G1 X0 Y10 E0.5\n\
             G1 X0 Y0 E0.5\n"
        ))
    }

    /// One wall's internal perimeter loops, the way a slicer emits them:
    /// concentric squares printed innermost first, since every mainstream
    /// slicer lays the external perimeter down last. Each loop is an extrusion
    /// width out from the one before and reached by its own travel, and each
    /// one's first extrusion is labelled `<tag><number>` in print order, so
    /// the highest number is the loop against the visible wall.
    fn wall(loops: usize, tag: &str) -> String {
        wall_of(loops, tag, 0.0, 10.0, 0.5)
    }

    fn wall_of(loops: usize, tag: &str, origin: f64, size: f64, flow: f64) -> String {
        let mut text = String::new();
        for index in 0..loops {
            let step = 0.45 * (loops - 1 - index) as f64;
            let near = origin + step;
            let far = origin + size - step;
            text.push_str(&format!("G1 X{near:.2} Y{near:.2} F9000\n"));
            text.push_str(&format!(
                "G1 X{far:.2} Y{near:.2} E{flow} ; {tag}{}\n",
                index + 1
            ));
            for (x, y) in [(far, far), (near, far), (near, near)] {
                text.push_str(&format!("G1 X{x:.2} Y{y:.2} E{flow}\n"));
            }
        }
        text
    }

    fn run(source: &str, config: &Config) -> String {
        apply(source, config).gcode
    }

    /// Each tagged loop in the output, paired with whether the nozzle was
    /// raised when it printed.
    fn loop_states(out: &str) -> Vec<(String, bool)> {
        let mut raised = false;
        let mut states = Vec::new();
        for line in out.lines() {
            let Some((body, tag)) = line.rsplit_once("; ") else {
                continue;
            };
            if let Some(note) = tag.strip_prefix(BRICK_STAMP) {
                match note {
                    "raised" => raised = true,
                    "reset" => raised = false,
                    _ => {}
                }
            } else if !body.trim().is_empty() {
                states.push((tag.to_owned(), raised));
            }
        }
        states
    }

    #[test]
    fn raises_every_other_internal_loop() {
        let source = middle_layer(&format!(";TYPE:Perimeter\n{}", wall(2, "loop")));
        let out = run(&source, &Config::default());
        assert!(
            out.contains("G1 Z0.500 F600 ; bricklayers brick raised"),
            "{out}"
        );
        assert!(
            out.contains("G1 Z0.400 F600 ; bricklayers brick reset"),
            "{out}"
        );
    }

    #[test]
    fn inserted_z_moves_carry_a_feedrate_and_hand_the_print_speed_back() {
        // A bare `G1 Z` inherits whatever `F` came last, which after a travel
        // slews the Z axis at travel speed. `F` is modal, so the print speed
        // has to be restored before the loop resumes.
        let source = middle_layer(
            ";TYPE:Perimeter\n\
             G1 X0.45 Y0.45 F9000\n\
             G1 X9.55 Y0.45 E0.5\n\
             G1 X9.55 Y9.55 E0.5\n\
             G1 X0.45 Y9.55 E0.5\n\
             G1 X0.45 Y0.45 E0.5\n\
             G1 X0 Y0 F9000\n\
             G1 F1800\n\
             G1 X10 Y0 E0.5\n\
             G1 X10 Y10 E0.5\n\
             G1 X0 Y10 E0.5\n\
             G1 X0 Y0 E0.5\n",
        );
        let out = run(&source, &Config::default());
        assert!(
            out.contains("G1 Z0.500 F600 ; bricklayers brick raised"),
            "{out}"
        );
        assert!(out.contains("G1 F1800 ; bricklayers brick resume"), "{out}");
    }

    #[test]
    fn inserted_z_moves_fall_back_when_the_file_never_moves_z_alone() {
        let source = relative(&format!(
            ";LAYER_CHANGE\n;TYPE:Perimeter\nG1 X0 Y0 Z0.2 F9000\n{}\
             ;LAYER_CHANGE\n;TYPE:Perimeter\nG1 X0 Y0 Z0.4 F9000\n{}\
             ;LAYER_CHANGE\n;TYPE:Solid infill\nG1 X0 Y0 Z0.6 F9000\nG1 X10 Y0 E0.5\n",
            wall(2, "loop"),
            wall(2, "above")
        ));
        let out = run(&source, &Config::default());
        assert!(
            out.contains("G1 Z0.300 F720 ; bricklayers brick raised"),
            "{out}"
        );
    }

    #[test]
    fn the_top_layer_caps_the_wall_flat() {
        // Raising here would stand a bead half a layer proud of the top
        // surface beside it, and meter it for half a gap that is a whole one.
        let source = relative(&format!(
            "{}{}{}{}{}",
            layer(0.2),
            layer(0.4),
            layer(0.6),
            format_args!(";TYPE:Perimeter\n{}", wall(2, "loop")),
            ";TYPE:Top solid infill\nG1 X40 Y0 E0.5\n"
        ));
        let out = run(&source, &Config::default());
        assert!(
            !out.contains("bricklayers brick raised"),
            "the top layer must stay on the plane: {out}"
        );
        assert!(
            out.contains("E0.25000 ; loop2"),
            "and still meter the half gap the raised loop below left: {out}"
        );
    }

    #[test]
    fn the_layer_that_tops_a_wall_caps_it_though_the_file_goes_on() {
        // What closes a part is solid infill laid over its walls, so the last
        // wall is a layer or more below the last layer. Measured on six real
        // slices: one to five layers below, so testing the layer count capped
        // nothing at all and left every part's topmost wall standing proud.
        let source = relative(&format!(
            "{}{}{}{}{}",
            layer(0.2),
            layer(0.4),
            format_args!(";TYPE:Perimeter\n{}", wall(2, "loop")),
            layer(0.6),
            ";TYPE:Top solid infill\nG1 X40 Y0 E0.5\n"
        ));
        let out = run(&source, &Config::default());
        assert!(
            !out.contains("bricklayers brick raised"),
            "the wall's top layer must stay on the plane: {out}"
        );
        assert!(
            out.contains("E0.25000 ; loop2"),
            "and still meter the half gap the raised loop below left: {out}"
        );
    }

    #[test]
    fn tracks_a_layer_z_that_arrives_inside_a_perimeter_region() {
        // Klipper flavour, and Orca with Z-hop off, fold the layer's Z into the
        // travel that reaches the first loop rather than emitting it alone.
        let source = relative(&format!(
            ";LAYER_CHANGE\n;TYPE:Perimeter\nG1 X0 Y0 Z0.2 F9000\n{}\
             ;LAYER_CHANGE\n;TYPE:Perimeter\nG1 X0 Y0 Z0.4 F9000\n{}\
             ;LAYER_CHANGE\n;TYPE:Perimeter\nG1 X0 Y0 Z0.6 F9000\n{}\
             ;LAYER_CHANGE\n;TYPE:Solid infill\nG1 X0 Y0 Z0.8 F9000\nG1 X10 Y0 E0.5\n",
            wall(2, "first"),
            wall(2, "second"),
            wall(2, "above"),
        ));
        let out = run(&source, &Config::default());
        assert!(
            out.contains("G1 Z0.300 F720 ; bricklayers brick raised"),
            "{out}"
        );
        assert!(
            out.contains("G1 Z0.500 F720 ; bricklayers brick raised"),
            "{out}"
        );
        assert!(
            !out.contains("G1 Z0.000"),
            "drove the nozzle into the bed: {out}"
        );
        assert!(
            !out.contains("G1 Z0.100"),
            "shifted off a stale layer Z: {out}"
        );
    }

    #[test]
    fn a_loop_that_does_not_touch_the_last_one_starts_a_new_contour() {
        // One region holding a three-loop wall and then a two-loop hole well
        // away from it. The wall's loops run beside each other and keep the
        // alternation going; the hole touches nothing, so it opens raised
        // despite following an odd loop count.
        let source = middle_layer(&format!(
            ";TYPE:Perimeter\n{}{}",
            wall_of(3, "wall", 0.0, 10.0, 0.5),
            wall_of(2, "hole", 50.0, 4.0, 0.5),
        ));
        let config = Config {
            extrusion_multiplier: 2.0,
            ..Config::default()
        };
        let out = run(&source, &config);
        assert!(out.contains("E1.00000 ; wall1"), "{out}");
        assert!(out.contains("E0.5 ; wall2"), "{out}");
        assert!(out.contains("E1.00000 ; wall3"), "{out}");
        assert!(out.contains("E0.5 ; hole1"), "{out}");
        assert!(out.contains("E1.00000 ; hole2"), "{out}");
    }

    #[test]
    fn an_open_wall_alternates_though_its_loops_do_not_nest() {
        // Most of a wall is not a closed ring. Where a slicer follows a curved
        // surface the loops are arcs, each one offset sideways from the last
        // and often longer than it, so neither encloses the other. They are
        // still the same wall, and grouping them by what they enclose leaves
        // almost every loop of a real print on its own.
        let mut arcs = String::new();
        for index in 0..4 {
            let x = 0.45 * index as f64;
            let reach = 4.0 + index as f64;
            arcs.push_str(&format!("G1 X{x:.2} Y0 F9000\n"));
            arcs.push_str(&format!("G1 X{x:.2} Y{reach:.2} E0.5 ; arc{}\n", index + 1));
        }
        let source = middle_layer(&format!(";TYPE:Perimeter\n{arcs}"));
        let outcome = apply(&source, &Config::default());
        assert_eq!(outcome.stats.loops, 5);
        assert_eq!(
            outcome.stats.raised, 2,
            "one wall, so every other arc: {}",
            outcome.gcode
        );
    }

    #[test]
    fn a_retraction_between_a_wall_s_own_loops_does_not_split_it() {
        // Slicers retract and hop between neighbouring loops of one wall
        // whenever the seams are far apart, which must not read as a new
        // contour: the two loops still have to alternate.
        let source = middle_layer(
            ";TYPE:Perimeter\n\
             G1 X0.45 Y0.45 F9000\n\
             G1 X9.55 Y0.45 E0.5 ; inner\n\
             G1 X9.55 Y9.55 E0.5\n\
             G1 X0.45 Y9.55 E0.5\n\
             G1 X0.45 Y0.45 E0.5\n\
             G1 E-0.8 F2100\n\
             G1 X20 Y20 F9000\n\
             G1 X0 Y0 F9000\n\
             G1 E0.8 F2100\n\
             G1 X10 Y0 E0.5 ; outer\n\
             G1 X10 Y10 E0.5\n\
             G1 X0 Y10 E0.5\n\
             G1 X0 Y0 E0.5\n",
        );
        let config = Config {
            extrusion_multiplier: 2.0,
            ..Config::default()
        };
        let out = run(&source, &config);
        assert!(out.contains("E0.5 ; inner"), "{out}");
        assert!(out.contains("E1.00000 ; outer"), "{out}");
    }

    #[test]
    fn a_layer_change_ends_the_region_it_interrupts() {
        // OrcaSlicer opens the next layer's wall with a stray segment before it
        // re-declares the region. Carrying the old region across the layer
        // change would buffer that segment as a perimeter loop of its own, at
        // the new layer's Z.
        let source = relative(&format!(
            ";LAYER_CHANGE\nG1 Z0.20 F600\n;TYPE:Perimeter\n{}\
             ;LAYER_CHANGE\nG1 Z0.40 F600\nG1 X20 Y20 E0.01 ; stray\n\
             ;TYPE:Perimeter\n{}",
            wall(2, "first"),
            wall(2, "second"),
        ));
        let outcome = apply(&source, &Config::default());
        assert!(
            outcome.gcode.contains("G1 X20 Y20 E0.01 ; stray\n"),
            "{}",
            outcome.gcode
        );
        assert_eq!(outcome.stats.loops, 4, "the stray segment is not a loop");
    }

    #[test]
    fn a_loop_that_opens_with_an_arc_is_raised_whole() {
        // Arc fitting replaces a run of short segments with one G2/G3, often at
        // the very start of a loop. Treating that as part of the travel would
        // print it before the nozzle rises, at the height of the loop below.
        let source = middle_layer(
            ";TYPE:Perimeter\n\
             G1 X0.45 Y0.45 F9000\n\
             G1 X9.55 Y0.45 E0.5\n\
             G1 X9.55 Y9.55 E0.5\n\
             G1 X0.45 Y9.55 E0.5\n\
             G1 X0.45 Y0.45 E0.5\n\
             G1 X0 Y0 F9000\n\
             G2 X10 Y0 I5 J1 E0.5 ; outer opens\n\
             G1 X10 Y10 E0.5\n\
             G1 X0 Y10 E0.5\n\
             G1 X0 Y0 E0.5\n",
        );
        let out = run(&source, &Config::default());
        let raise = out
            .find("G1 Z0.500 F600 ; bricklayers brick raised")
            .expect("the wall is raised");
        let arc = out.find("; outer opens").expect("arc kept");
        assert!(
            raise < arc,
            "the arc opens the loop, so it rises with it:\n{out}"
        );
    }

    #[test]
    fn a_contour_holding_one_loop_is_raised_against_the_wall_that_shows() {
        // A lone loop has no internal neighbour, but it was inset from an
        // external perimeter, so the visible wall is what it staggers against.
        let source = middle_layer(&format!(";TYPE:Perimeter\n{}", wall(1, "lone")));
        let outcome = apply(&source, &Config::default());
        assert_eq!(
            loop_states(&outcome.gcode),
            vec![("lone1".to_owned(), true)]
        );
        // The second is the fixture's own wall on the layer above.
        assert_eq!(outcome.stats.loops, 2);
        assert_eq!(outcome.stats.raised, 1);
    }

    #[test]
    fn a_solid_wall_three_beads_thick_bricks_its_single_inner_bead() {
        // The case a thin rib produces: the visible wall wraps both faces and
        // one internal loop runs down the middle. Raising it keys the rib to
        // the wall on either side, and the wall itself must stay on the plane.
        let source = middle_layer(
            ";TYPE:Perimeter\n\
             G1 X0.70 Y0.68 F9000\n\
             G1 X39.30 Y0.68 E1.0 ; rib1\n\
             ;TYPE:External perimeter\n\
             G1 X0.22 Y0.22 F9000\n\
             G1 X39.78 Y0.22 E1.0 ; skin1\n\
             G1 X39.78 Y1.13 E0.1\n\
             G1 X0.22 Y1.13 E1.0\n\
             G1 X0.22 Y0.22 E0.1\n",
        );
        let outcome = apply(&source, &Config::default());
        assert_eq!(
            loop_states(&outcome.gcode),
            vec![("rib1".to_owned(), true), ("skin1".to_owned(), false)],
            "{}",
            outcome.gcode
        );
        assert_eq!(outcome.stats.raised, 1);
    }

    #[test]
    fn a_lone_hole_is_bricked_beside_a_wall_in_the_same_region() {
        // Numbering restarts per contour, so a two-loop wall and the
        // single-loop hole beside it are each anchored on their own
        // external-adjacent end.
        let source = middle_layer(&format!(
            ";TYPE:Perimeter\n{}{}",
            wall_of(2, "wall", 0.0, 10.0, 0.5),
            wall_of(1, "hole", 50.0, 4.0, 0.5),
        ));
        let config = Config {
            extrusion_multiplier: 2.0,
            ..Config::default()
        };
        let outcome = apply(&source, &config);
        assert!(
            outcome.gcode.contains("E1.00000 ; wall2"),
            "{}",
            outcome.gcode
        );
        assert!(
            outcome.gcode.contains("E1.00000 ; hole1"),
            "{}",
            outcome.gcode
        );
        assert_eq!(outcome.stats.loops, 4);
        assert_eq!(outcome.stats.raised, 2);
    }

    #[test]
    fn restores_z_before_leaving_the_perimeter_region() {
        let source = middle_layer(&format!(
            ";TYPE:Perimeter\n{};TYPE:Solid infill\nG1 X40 Y0 E0.5\n",
            wall(2, "loop")
        ));
        let out = run(&source, &Config::default());
        let reset = out
            .find("G1 Z0.400 F600 ; bricklayers brick reset")
            .expect("reset emitted");
        let infill = out.find(";TYPE:Solid infill").expect("marker kept");
        assert!(reset < infill, "Z must drop before infill starts:\n{out}");
    }

    #[test]
    fn external_perimeters_are_untouched() {
        let source = middle_layer(";TYPE:External perimeter\nG1 X10 Y0 E0.5\nG1 X20 Y0 E0.5\n");
        assert!(!run(&source, &Config::default()).contains("bricklayers"));
    }

    #[test]
    fn scales_extrusion_of_raised_loops_only() {
        let source = middle_layer(&format!(
            ";TYPE:Perimeter\n{}",
            wall_of(2, "loop", 0.0, 10.0, 1.0)
        ));
        let config = Config {
            extrusion_multiplier: 1.5,
            ..Config::default()
        };
        let out = run(&source, &config);
        assert!(out.contains("E1.50000 ; loop2"), "{out}");
        assert!(out.contains("E1 ; loop1"), "{out}");
    }

    #[test]
    fn the_default_multiplier_leaves_a_raised_loop_metered_as_sliced() {
        let source = middle_layer(&format!(
            ";TYPE:Perimeter\n{}",
            wall_of(2, "loop", 0.0, 10.0, 1.0)
        ));
        let outcome = apply(&source, &Config::default());
        assert_eq!(outcome.stats.multiplier_filament, 0.0);
        assert!(outcome.gcode.contains("E1 ; loop2"), "{}", outcome.gcode);
        assert!(outcome.gcode.contains("E1 ; loop1"), "{}", outcome.gcode);
    }

    #[test]
    fn the_multiplier_is_booked_apart_from_the_flow_it_scales() {
        let source = middle_layer(&format!(
            ";TYPE:Perimeter\n{}",
            wall_of(2, "loop", 0.0, 10.0, 1.0)
        ));
        let config = Config {
            extrusion_multiplier: 1.05,
            ..Config::default()
        };
        let outcome = apply(&source, &config);
        assert!(
            outcome.gcode.contains("E1.05000 ; loop2"),
            "{}",
            outcome.gcode
        );
        assert!(outcome.gcode.contains("E1 ; loop1"), "{}", outcome.gcode);
        // Only the multiplier's own share, not the whole raised loop: four
        // moves of 1 mm at 1.05 add 0.2, whatever else the file books.
        assert!(
            (outcome.stats.multiplier_filament - 0.2).abs() < 1e-9,
            "{:?}",
            outcome.stats
        );
        assert!(
            outcome.stats.filament > outcome.stats.raised_filament,
            "{:?}",
            outcome.stats
        );
    }

    #[test]
    fn first_and_last_layers_meter_the_gaps_the_stagger_leaves() {
        let wall = format!(";TYPE:Perimeter\n{}", wall_of(2, "loop", 0.0, 10.0, 1.0));
        let source = relative(&format!("{}{wall}{}{wall}", layer(0.2), layer(0.4)));
        let config = Config {
            extrusion_multiplier: 9.0,
            ..Config::default()
        };
        let out = run(&source, &config);
        // The first layer's raised bead reaches down to the bed, so it spans a
        // layer and a half; the last one fills only the half the layer below
        // left open.
        assert!(out.contains("E1.50000"), "first layer 1.5x missing:\n{out}");
        assert!(out.contains("E0.50000"), "last layer 0.5x missing:\n{out}");
    }

    #[test]
    fn a_thicker_first_layer_gets_the_flow_its_own_gap_needs() {
        // A 0.6 mm first layer under 0.8 mm layers: the raised bead spans 0.6
        // plus the 0.4 shift, so it needs 1.667x, not the 1.5x that only holds
        // when the two heights are equal.
        let source = format!(
            "; layer_height = 0.8\n\
             ; first_layer_height = 0.6\n\
             M83\n\
             ;LAYER_CHANGE\n\
             G1 Z0.60 F600\n\
             ;TYPE:Perimeter\n{}\
             ;LAYER_CHANGE\n\
             G1 Z1.40 F600\n\
             ;TYPE:Perimeter\n{}\
             ;LAYER_CHANGE\n\
             G1 Z2.20 F600\n\
             ;TYPE:Solid infill\n\
             G1 X40 Y0 E1.0\n",
            wall_of(2, "loop", 0.0, 10.0, 1.0),
            wall_of(2, "above", 0.0, 10.0, 1.0)
        );
        let out = run(&source, &Config::default());
        assert!(
            out.contains("G1 Z1.000 F600 ; bricklayers brick raised"),
            "{out}"
        );
        assert!(out.contains("E1.66667 ; loop2"), "{out}");
        assert!(
            !out.contains("E1.50000"),
            "1.5x assumes the first layer is a layer height:\n{out}"
        );
    }

    #[test]
    fn the_first_layer_height_may_be_given_when_the_file_omits_it() {
        let wall = format!(";TYPE:Perimeter\n{}", wall_of(2, "loop", 0.0, 10.0, 1.0));
        let source = relative(&format!("{}{wall}{}{wall}", layer(0.2), layer(0.4)));
        let config = Config {
            first_layer_height: Some(0.4),
            ..Config::default()
        };
        // 0.4 mm laid down, raised 0.1: (0.4 + 0.1) / 0.4.
        assert!(run(&source, &config).contains("E1.25000"));
    }

    #[test]
    fn reordering_groups_unraised_loops_first() {
        let source = middle_layer(&format!(";TYPE:Perimeter\n{}", wall(3, "loop")));
        let config = Config {
            reorder_loops: true,
            ..Config::default()
        };
        let out = run(&source, &config);
        // Numbering runs outwards from the visible wall, so of three loops the
        // middle one is the only one left on the plane.
        let flat = out.find("loop2").expect("middle loop kept");
        let first_raised = out.find("loop1").expect("innermost loop kept");
        assert!(
            flat < first_raised,
            "unraised loop should print first:\n{out}"
        );
        assert!(out.contains("G1 X0.90 Y0.90 F9000"), "travel kept:\n{out}");
    }

    /// The region buffer, the loop list and the replay order are all reused
    /// between regions, so a second wall in the same layer has to be grouped
    /// and numbered from scratch rather than from whatever the first left.
    #[test]
    fn a_second_region_in_a_layer_is_grouped_on_its_own() {
        let source = middle_layer(&format!(
            ";TYPE:Perimeter\n{}\
             ;TYPE:External perimeter\nG1 X0 Y0 F9000\nG1 X10 Y0 E0.5\n\
             ;TYPE:Perimeter\n{}",
            wall_of(2, "near", 0.0, 10.0, 0.5),
            wall_of(3, "far", 40.0, 10.0, 0.5),
        ));
        let out = run(&source, &Config::default());

        // Numbering runs outwards from the loop against the visible wall,
        // which each slicer prints last, so the raise alternates from there.
        assert_eq!(
            loop_states(&out),
            [
                ("near1".to_owned(), false),
                ("near2".to_owned(), true),
                ("far1".to_owned(), true),
                ("far2".to_owned(), false),
                ("far3".to_owned(), true),
            ],
            "{out}"
        );
    }

    #[test]
    fn reordering_two_regions_keeps_each_ones_loops_together() {
        let source = middle_layer(&format!(
            ";TYPE:Perimeter\n{}\
             ;TYPE:External perimeter\nG1 X0 Y0 F9000\nG1 X10 Y0 E0.5\n\
             ;TYPE:Perimeter\n{}",
            wall_of(3, "near", 0.0, 10.0, 0.5),
            wall_of(3, "far", 40.0, 10.0, 0.5),
        ));
        let config = Config {
            reorder_loops: true,
            ..Config::default()
        };
        let out = run(&source, &config);

        let tags: Vec<String> = loop_states(&out).into_iter().map(|(tag, _)| tag).collect();
        assert_eq!(
            tags,
            ["near2", "near1", "near3", "far2", "far1", "far3"],
            "each region's unraised loop should lead its own group:\n{out}"
        );
    }

    /// Slicers scatter their own annotations through a wall. They are not
    /// region markers, so they must neither end the region nor be dropped.
    #[test]
    fn a_slicer_annotation_inside_a_wall_is_replayed_in_place() {
        let source = middle_layer(&format!(
            ";TYPE:Perimeter\n; LINE_WIDTH: 0.42\n{}",
            wall(2, "loop")
        ));
        let out = run(&source, &Config::default());

        assert!(
            out.contains("; LINE_WIDTH: 0.42"),
            "annotation kept:\n{out}"
        );
        assert_eq!(
            loop_states(&out)
                .into_iter()
                .filter(|(tag, _)| tag.starts_with("loop"))
                .collect::<Vec<_>>(),
            [("loop1".to_owned(), false), ("loop2".to_owned(), true)],
            "the annotation must not split the wall:\n{out}"
        );
    }

    #[test]
    fn absolute_extrusion_stays_continuous() {
        let source = format!(
            "; layer_height = 0.2\nM82\n{}{};TYPE:Perimeter\n\
             G1 X0.45 Y0.45 F9000\n\
             G1 X9.55 Y0.45 E1.0 ; loop1\n\
             G1 X9.55 Y9.55 E2.0\n\
             G1 X0.45 Y9.55 E3.0\n\
             G1 X0.45 Y0.45 E4.0\n\
             G1 X0 Y0 F9000\n\
             G1 X10 Y0 E5.0 ; loop2\n\
             G1 X10 Y10 E6.0\n\
             G1 X0 Y10 E7.0\n\
             G1 X0 Y0 E8.0\n\
             ;TYPE:Solid infill\n\
             G1 X30 Y0 E9.0\n{}\
             ;TYPE:Perimeter\n\
             G1 X0 Y0 F9000\n\
             G1 X10 Y0 E10.0\n\
             G1 X10 Y10 E11.0\n",
            layer(0.2),
            layer(0.4),
            layer(0.6),
        );
        let config = Config {
            extrusion_multiplier: 2.0,
            ..Config::default()
        };
        let out = run(&source, &config);
        // The raised loop's four 1 mm moves are doubled, so everything after
        // them has to carry the four extra millimetres they added to the
        // absolute stream, including the infill move left at 1x.
        assert!(out.contains("G1 X9.55 Y0.45 E1.0 ; loop1"), "{out}");
        assert!(out.contains("G1 X10 Y0 E6.00000 ; loop2"), "{out}");
        assert!(out.contains("G1 X30 Y0 E13.00000"), "{out}");
    }

    /// Whether a line has to be written again is whether the value it should
    /// carry differs from the one it already has. Asking a global drift flag
    /// instead is wrong inside a buffered region: the region is read to its
    /// end before any of it is emitted, so the input position sits ahead of
    /// the output and the two coincide by accident every so often. The line
    /// where they met came out carrying its original, now stale, absolute
    /// value — on a Cura-flavoured file the extruder ran 0.6 mm backwards
    /// mid-wall and then asked for a double-length move to catch up.
    #[test]
    fn an_absolute_stream_never_runs_backwards() {
        // The first layer's raised bead is metered thicker, which shifts every
        // value after it and sets up the coincidence.
        let mut source = String::from("; layer_height = 0.2\nM82\nG92 E0\n");
        let mut e = 0.0;
        for index in 0..6 {
            source.push_str(&layer(0.2 + f64::from(index) * 0.2));
            source.push_str(";TYPE:Perimeter\n");
            for inset in [0.9_f64, 0.45] {
                source.push_str(&format!("G1 X{inset:.2} Y{inset:.2} F9000\n"));
                let far = 20.0 - inset;
                for (x, y) in [(far, inset), (far, far), (inset, far), (inset, inset)] {
                    e += 0.6;
                    source.push_str(&format!("G1 X{x:.3} Y{y:.3} E{e:.5}\n"));
                }
            }
            source.push_str(";TYPE:Solid infill\nG1 X2 Y2 F9000\n");
            e += 1.2;
            source.push_str(&format!("G1 X18 Y18 E{e:.5}\n"));
        }
        let out = run(&source, &Config::default());

        let mut position = 0.0;
        let mut moves = 0;
        for line in out.lines() {
            let parsed = Line::parse(line);
            let Some(value) = parsed.e.filter(|_| parsed.draws()) else {
                continue;
            };
            moves += 1;
            assert!(
                value >= position,
                "{line} pulls the filament back from {position}:\n{out}"
            );
            assert!(
                value - position <= 1.5,
                "{line} asks for {} mm in one move:\n{out}",
                value - position
            );
            position = value;
        }
        assert!(moves > 50, "expected the whole file to be checked");
    }

    #[test]
    fn numbering_follows_the_wall_order_the_slicer_used() {
        // Printed the other way round, the loop against the visible wall is
        // the first one out of the nozzle rather than the last.
        let source = middle_layer(&format!(";TYPE:Perimeter\n{}", wall(2, "loop")));
        let config = Config {
            external_perimeters_first: true,
            extrusion_multiplier: 2.0,
            ..Config::default()
        };
        let out = run(&source, &config);
        assert!(out.contains("E1.00000 ; loop1"), "{out}");
        assert!(out.contains("E0.5 ; loop2"), "{out}");
    }

    /// A layer height that is not a length would become half a shift and drive
    /// the nozzle down into the layer below, or write `ZNaN` into the file.
    /// Every source of one is filtered, this included.
    #[test]
    fn a_layer_height_that_is_not_a_length_is_ignored() {
        let source = middle_layer(&format!(";TYPE:Perimeter\n{}", wall(2, "loop")));
        let sane = run(&source, &Config::default());

        for height in [0.0, -0.4, f64::NAN, f64::INFINITY] {
            let config = Config {
                layer_height: Some(height),
                ..Config::default()
            };
            let outcome = apply(&source, &config);
            assert_eq!(
                outcome.gcode, sane,
                "--layer-height {height} should fall back to the file's own"
            );
            for line in outcome.gcode.lines() {
                if let Some(z) = Line::parse(line).z {
                    assert!(z.is_finite() && z >= 0.0, "{line}");
                }
            }
        }
    }

    /// Cura resets the extruder origin periodically to keep `E` from growing
    /// without bound, and the reset can land inside a wall. The region's own
    /// moves have not been metered out when it arrives, so replaying them
    /// after it measured their absolute positions from the wrong zero: the
    /// first move after `G92 E0` asked for 2.5 mm of filament in one go.
    #[test]
    fn a_g92_inside_a_wall_keeps_the_absolute_stream_honest() {
        let source = format!(
            "; layer_height = 0.2\nM82\n{}{}{};TYPE:Perimeter\n\
             G1 X0.45 Y0.45 F9000\n\
             G1 X9.55 Y0.45 E1.0\n\
             G1 X9.55 Y9.55 E2.0\n\
             G92 E0\n\
             G1 X0 Y0 F9000\n\
             G1 X10 Y0 E1.0\n\
             G1 X10 Y10 E2.0\n",
            layer(0.2),
            layer(0.4),
            layer(0.6),
        );
        let config = Config {
            extrusion_multiplier: 1.3,
            ..Config::default()
        };
        let out = run(&source, &config);

        // The origin line is not an extrusion and must survive untouched.
        assert!(out.contains("\nG92 E0\n"), "{out}");

        // Every absolute value after the reset is measured from the new zero,
        // so none of them may jump.
        let after = out.split("\nG92 E0\n").nth(1).expect("the reset is kept");
        let mut position = 0.0;
        for line in after.lines() {
            let parsed = Line::parse(line);
            if !parsed.draws() {
                continue;
            }
            if let Some(e) = parsed.e {
                assert!(
                    e >= position && e - position <= 2.0,
                    "{line} asks for {} mm in one move:\n{out}",
                    e - position
                );
                position = e;
            }
        }
    }

    /// A file sliced to complete individual objects builds each one from the
    /// bed up, so it holds several first and last layers rather than one pair.
    /// Metering only the file's own leaves every later object's bed layer
    /// starved and bricks the top of every object but the last.
    #[test]
    fn every_object_gets_its_own_first_and_last_layer() {
        let mut source = String::from("; layer_height = 0.2\nM83\n");
        for object in 1..=2 {
            for index in 0..4 {
                source.push_str(&layer(0.2 + f64::from(index) * 0.2));
                source.push_str(";TYPE:Perimeter\n");
                source.push_str(&wall_of(
                    2,
                    &format!("o{object}L{index}loop"),
                    0.0,
                    10.0,
                    0.5,
                ));
            }
        }
        let out = run(&source, &Config::default());

        // The bead of a layer laid on the bed spans the first layer height
        // plus the shift, so it is metered at 1.5x; the bead that caps an
        // object fills the half gap the one below left, so it is metered at
        // 0.5x. Everything between is left as the slicer metered it.
        let flow = |tag: &str| {
            out.lines()
                .find(|line| line.ends_with(tag))
                .map(|line| Line::parse(line).e.expect("an extrusion"))
                .unwrap_or_else(|| panic!("{tag} missing from:\n{out}"))
        };
        for object in 1..=2 {
            assert_eq!(
                flow(&format!("o{object}L0loop2")),
                0.75,
                "object {object} bed layer"
            );
            assert_eq!(
                flow(&format!("o{object}L3loop2")),
                0.25,
                "object {object} top layer"
            );
            assert_eq!(
                flow(&format!("o{object}L1loop2")),
                0.5,
                "object {object} middle"
            );
        }
    }

    #[test]
    fn g92_resets_the_extrusion_offset() {
        let source = format!(
            "M82\n{};TYPE:Perimeter\nG1 X10 Y0 E1.0\n;TYPE:Solid infill\nG92 E0\nG1 X20 Y0 E1.0\n",
            layer(0.2),
        );
        let config = Config {
            extrusion_multiplier: 2.0,
            ..Config::default()
        };
        let out = run(&source, &config);
        assert!(out.contains("G1 X20 Y0 E1.0\n"), "{out}");
    }

    #[test]
    fn gcode_without_perimeters_is_unchanged() {
        let source = "M83\nG1 Z0.2\nG1 X1 Y1 E0.1\nM104 S0\n";
        assert_eq!(run(source, &Config::default()), source);
    }

    #[test]
    fn empty_input_produces_empty_output() {
        assert_eq!(run("", &Config::default()), "");
    }

    #[test]
    fn reports_what_it_did() {
        let source = middle_layer(&format!(";TYPE:Perimeter\n{}", wall(2, "loop")));
        let stats = apply(&source, &Config::default()).stats;
        assert_eq!(stats.loops, 3);
        assert_eq!(stats.raised, 1);
        assert_eq!(stats.layers, 3);
        assert_eq!(stats.layer_height, 0.2);
        assert!(stats.layer_height_detected);
    }
}
