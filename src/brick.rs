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
use crate::footprint::{self, Arc, Cells};
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

/// Layers a raised column takes to climb to its full offset.
///
/// Displacing a column upwards opens a half-layer void beneath it that has to
/// be extruded once, and the bead carrying it spans its own layer plus the
/// whole climb. Asking one bead for all of it leaves the nozzle half a layer
/// clear of the surface it is laying against, so it presses nothing and the
/// extra flow spreads sideways instead of building height. Climbing costs the
/// same filament and asks no bead to span more than a quarter of a layer
/// beyond what the slicer metered it for.
///
/// The climb starts above the bed: a layer laid on the build plate has no seam
/// under it to stagger, cannot be pressed against a surface that is not a
/// layer, and is the face of the part that shows. On a Benchy the whole of the
/// bottom nameplate is one layer deep, and raising it filled the letters in.
const RAMP: usize = 2;

/// How much of a loop has to have nothing above it before the loop is laid
/// flat instead of raised.
///
/// A raised bead stands half a layer proud, so anything the slicer prints over
/// it at the next plane fills half the gap it was metered for. Where a wall
/// ends under a solid surface that is around twice the flow the surface has
/// room for: measured on a bushing whose shoulder closes at 3 mm, 293.8 mm of
/// the 399.0 mm top surface above it sat on a bead 0.1 mm proud.
///
/// The threshold is high on purpose. Capping a loop whose column carries on
/// above would leave the layer above it metered against a step that is no
/// longer there, so only a loop that has genuinely run out is worth flattening.
/// Measured over three real slices the two cases barely overlap: 91 to 97% of
/// loops have a wall above almost all of them, and what is left is almost all
/// uncovered end to end.
const CAP_SHARE: f64 = 0.75;

/// Most lines held back between one region's last bead and the next region's
/// first, so that a wall's opening travel can carry its height.
///
/// A real lead is a handful of lines — a travel, a hop restore, a prime and
/// the markers. The cap is what keeps the promise that nothing larger than one
/// region is ever buffered, whatever a file puts between two extrusions.
const TAIL: usize = 64;

#[derive(Clone, Copy, Debug)]
pub struct Config {
    /// Layer height in mm, used for every layer. `None` takes each layer's own
    /// height from the file, which is the only right answer where the slicer
    /// varied it.
    pub layer_height: Option<f64>,
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
    /// Smallest and largest half-layer any raise was taken from, or `None`
    /// where nothing was raised. The two differ only on an adaptive slice.
    pub raise: Option<(f64, f64)>,
    pub layers: usize,
    pub loops: usize,
    pub raised: usize,
    /// Loops laid flat because nothing stood on them, which would otherwise
    /// have been buried under a bead metered for a full layer.
    pub capped: usize,
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
        layer_height: pass.height,
        layer_height_detected: config.layer_height.is_some() || survey.layer_height_detected,
        raise: pass.raise,
        layers: survey.layers,
        loops: pass.loops_seen,
        raised: pass.raised,
        capped: pass.capped,
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
    /// Where the move ends, with the axes it left unnamed carried forward, so
    /// a loop's path can be walked without re-reading the region.
    at: (f64, f64),
    /// Centre and direction of a `G2`/`G3`, so its path is followed round
    /// rather than cut across.
    arc: Option<Arc>,
    extrudes: bool,
    /// True where the line decides where the nozzle is next, so nothing after
    /// it in a lead can undo a height set on it.
    positions: bool,
    /// True where a height change can ride this line instead of stopping the
    /// toolhead for one of its own. Extrusions and wipes are excluded — their
    /// path is laid against the layer below — and so is a line that already
    /// carries a comment, which is where the stamp has to go.
    carries: bool,
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
    /// True where nothing stands on this loop on the next layer, so it has to
    /// finish flat whatever the parity says.
    capped: bool,
    /// Layers this loop's own column has stood for. Zero where the column
    /// begins on this layer, so its first bead climbs from the plane rather
    /// than being raised to an offset nothing under it earned.
    steps: usize,
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
    /// Where each layer's walls have nothing above them, from the survey.
    uncovered: &'a [Cells],
    unsupported: &'a [Cells],
    layer_markers: bool,
    /// Layer height to use where the file measured none, which is every layer
    /// of a file sliced at a fixed height.
    height: f64,
    /// Height the file measured for each layer, empty unless the slicer varied
    /// them. A raise is half of the layer it belongs to, so an adaptive slice
    /// has as many raises as it has heights.
    heights: Vec<f64>,
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
    capped: usize,
    /// Where the nozzle stands in the plane, with the axes each move left
    /// unnamed carried forward, and where it stood when the buffered region
    /// began.
    at: (f64, f64),
    entry: (f64, f64),
    /// Smallest and largest half-layer a raise was taken from, so a report can
    /// give a range rather than one number the file never used.
    raise: Option<(f64, f64)>,
    filament: f64,
    raised_filament: f64,
    multiplier_filament: f64,
}

impl<'a, W: Write> Pass<'a, W> {
    fn new(out: W, config: &'a Config, survey: &'a Survey) -> Self {
        let uniform = config.layer_height.filter(is_a_height);
        Self {
            config,
            object_starts: survey.object_starts.clone(),
            object_tops: survey.object_tops.clone(),
            uncovered: &survey.uncovered,
            unsupported: &survey.unsupported,
            layer_markers: survey.layer_markers,
            height: uniform.unwrap_or(survey.layer_height),
            // A height given on the command line is the one the caller wants
            // used, so it stands in for the measurement rather than beside it.
            heights: match uniform {
                Some(_) => Vec::new(),
                None => survey.layer_heights.clone(),
            },
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
            capped: 0,
            at: (0.0, 0.0),
            entry: (0.0, 0.0),
            raise: None,
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
                // Only a region that buffered loops has to be metered out
                // here. A tail held back for the next region to ride keeps its
                // place ahead of the marker instead.
                if !self.loops.is_empty() {
                    self.flush()?;
                }
                self.feature = feature;
                return self.keep(raw, line, self.at);
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

        // A slicer names only the axes that change, so a move starts wherever
        // the last one left off.
        let from = self.at;
        if line.draws() {
            self.at = (line.x.unwrap_or(from.0), line.y.unwrap_or(from.1));
        }

        if self.feature == Feature::InternalPerimeter {
            if self.buffer.is_empty() {
                self.entry = from;
            }
            self.buffer(raw, line);
            return Ok(());
        }

        self.keep(raw, line, from)
    }

    /// Buffers a line that a region opening after it might still need, or
    /// writes it straight out.
    ///
    /// The travel that reaches a region's first loop is emitted before the
    /// `; FEATURE:` marker that opens the region, so without holding it back
    /// the first loop has nothing to carry its height and needs a `G1 Z` of
    /// its own — which stops the toolhead on the loop's start point, primed,
    /// which is the seam. Only what can sit between one region's last bead and
    /// the next region's first is held: travels, height moves, and the
    /// comments among them.
    fn keep(&mut self, raw: &str, line: Line<'_>, from: (f64, f64)) -> io::Result<()> {
        let lays = (line.x.is_some() || line.y.is_some()) && line.e.is_some();
        let holds = self.loops.is_empty()
            && self.buffer.len() < TAIL
            && (line.marker().is_some() || (line.draws() && !lays));
        if holds {
            if self.buffer.is_empty() {
                self.entry = from;
            }
            self.buffer(raw, line);
            return Ok(());
        }

        // Anything else ends the tail, and it has to be written out before the
        // line that ended it.
        self.flush()?;
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
        let positions = line.is_move() && (line.is_xy_move() || line.z.is_some());
        let carries = positions && line.e.is_none() && line.comment().is_none();

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
            at: self.at,
            arc: line.arc(),
            extrudes,
            positions,
            carries,
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
            capped: false,
            steps: 0,
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
        self.mark_columns();

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

            // A loop with nothing standing on it stays on the plane however
            // the parity fell: raising it would leave a bead half a layer
            // proud of whatever the slicer prints over it next, into a gap it
            // metered for a whole layer. `extrusion_factor` meters the half
            // gap the column below already filled.
            let offset = if current.raised {
                self.offset(current.steps, current.capped)
            } else {
                0.0
            };
            let raise = offset > 0.0;
            let target = self.layer_z + offset;
            let carrier = self.carrier(current.lead, current.body, target);
            for at in current.lead..current.body {
                match carrier {
                    Some(at_) if at_ == at => self.ride(at, target, raise)?,
                    _ => self.replay(at, 1.0)?,
                }
            }
            // After the lead, so a slicer's own Z-hop restore cannot undo it.
            // A no-op where the carrier already took the nozzle there.
            self.move_z(target, raise)?;
            let factor = self.extrusion_factor(current.raised, current.steps, current.capped);
            if raise {
                let half = self.height() / 2.0;
                self.raise = Some(match self.raise {
                    Some((low, high)) => (low.min(half), high.max(half)),
                    None => (half, half),
                });
                self.meter(current.body, end, factor, current.steps, current.capped);
            }
            // The nozzle has to come back down before whatever the slicer
            // prints next, and the travel that leaves the region can carry it
            // just as well as the lead carried the way up.
            let closing = (position + 1 == self.loops.len() && raise)
                .then(|| self.carrier(current.body, end, self.layer_z))
                .flatten();
            for at in current.body..end {
                match closing {
                    Some(at_) if at_ == at => self.ride(at, self.layer_z, false)?,
                    _ => self.replay(at, factor)?,
                }
            }

            last_raised = raise;
            self.loops_seen += 1;
            self.raised += usize::from(raise);
            self.capped += usize::from(current.raised && current.capped);
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

    /// The buffered move in `from..to` that can take the nozzle to `z` on its
    /// way, or `None` where one of its own is needed.
    ///
    /// A `G1 Z` between two loops stops the toolhead dead: it names no other
    /// axis, so the planner cannot blend it with the moves on either side, and
    /// the nozzle sits still and primed over the loop's start point while the
    /// axis crawls. Every loop start is the seam, and an aligned seam stacks
    /// them into one column, so the ooze from all of them lands in a line.
    /// Measured on a 77-layer PETG part: 679 such stops, 67.5 mm of Z travel,
    /// 13.5 s of standing still on a 12 m print, 145 of them landing on the
    /// visible wall's own start point.
    ///
    /// The last move of the range is the one to use, since anything after it
    /// would override the height. It has to be a plain move: an extrusion or a
    /// wipe follows the layer below and cannot be tilted, and a line that
    /// already carries a comment has no room for the stamp that stops the file
    /// being processed twice.
    fn carrier(&self, from: usize, to: usize, z: f64) -> Option<usize> {
        // Nothing to carry where the lead already leaves the nozzle there.
        let landing = self.buffer[from..to]
            .iter()
            .rev()
            .find_map(|buffered| buffered.z)
            .or(self.nozzle_z);
        if landing == Some(z) {
            return None;
        }
        let index = to
            - self.buffer[from..to]
                .iter()
                .rev()
                .position(|b| b.positions)?
            - 1;
        let carrier = self.buffer[index];
        // Never ride a move the slicer put above the plane on purpose:
        // pulling a Z-hop down to printing height would drag the nozzle
        // through what it was lifted to clear.
        (carrier.carries && carrier.z.is_none_or(|had| had <= z)).then_some(index)
    }

    /// Replays a buffered move with its height set to `z`, in place of the
    /// `G1 Z` that would otherwise have been inserted after it.
    fn ride(&mut self, index: usize, z: f64, raised: bool) -> io::Result<()> {
        let buffered = self.buffer[index];
        self.nozzle_z = Some(z);
        if let Some(rate) = buffered.f {
            self.feedrate = Some(rate);
        }
        let note = if raised { "raised" } else { "reset" };
        let Self { arena, out, .. } = self;
        // No `E` word to rescale: `Buffered::carries` is only true without one.
        Line::parse(&arena[buffered.start..buffered.end]).write_z(out, z)?;
        writeln!(out, " ; {BRICK_STAMP}{note}")
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

    /// Flow a loop's bead needs, as a multiple of what the slicer metered it
    /// for.
    fn extrusion_factor(&self, raised: bool, steps: usize, capped: bool) -> f64 {
        if !raised {
            return 1.0;
        }
        self.span(steps, capped) * self.multiplier(steps, capped)
    }

    /// How far a raised bead reaches, as a multiple of its own layer's height.
    ///
    /// It starts on top of whatever its column left on the layer below and
    /// ends at the nozzle, so the span is this layer's height plus the ground
    /// its offset gained over the one beneath it. Where the two offsets match
    /// it spans exactly one layer and the arithmetic is skipped rather than
    /// trusted: `(h + x) - x` is not `h` in binary.
    fn span(&self, steps: usize, capped: bool) -> f64 {
        let offset = self.offset(steps, capped);
        let below = self.rise_below(steps);
        if offset == below {
            return 1.0;
        }
        let height = self.height();
        (height + offset - below) / height
    }

    /// [`Config::extrusion_multiplier`] where it has anything to compensate
    /// for, which is a column standing at its full offset. A bead still
    /// climbing, or one capping a wall, is already metered for the step it
    /// bridges.
    fn multiplier(&self, steps: usize, capped: bool) -> f64 {
        if self.settled(steps, capped) {
            self.config.extrusion_multiplier
        } else {
            1.0
        }
    }

    /// True where a raised column has finished climbing and is not being
    /// capped.
    fn settled(&self, steps: usize, capped: bool) -> bool {
        steps > RAMP && !capped
    }

    /// Height of the layer being printed.
    fn height(&self) -> f64 {
        self.height_at(self.layer)
    }

    /// What `layer` was sliced at, falling back to the one height that
    /// describes files the slicer did not vary.
    fn height_at(&self, layer: usize) -> f64 {
        self.heights
            .get(layer)
            .copied()
            .filter(is_a_height)
            .unwrap_or(self.height)
    }

    /// How far a raised loop on `layer` stands above the plane once its column
    /// has climbed for `steps` layers.
    ///
    /// Half of the layer's own height, so an adaptive slice staggers each
    /// layer against the seam it actually has rather than against an average
    /// no layer was printed at.
    fn rise_at(&self, steps: usize, layer: usize) -> f64 {
        self.height_at(layer) / 2.0 * steps.min(RAMP) as f64 / RAMP as f64
    }

    /// The offset this loop takes, once its own column has stood for `steps`
    /// layers.
    fn offset(&self, steps: usize, capped: bool) -> f64 {
        if capped {
            0.0
        } else {
            self.rise_at(steps, self.layer)
        }
    }

    /// The offset the same column was left standing at on the layer below,
    /// measured from that layer's height rather than this one's.
    fn rise_below(&self, steps: usize) -> f64 {
        match steps {
            0 => 0.0,
            steps => self.rise_at(steps - 1, self.layer - 1),
        }
    }

    /// Layers printed since this object's first. A file that completes objects
    /// one at a time builds each from the bed up, so it has several.
    fn steps(&self) -> usize {
        let start = self
            .object_starts
            .iter()
            .rev()
            .find(|&&start| start <= self.layer)
            .copied()
            .unwrap_or(0);
        self.layer - start
    }

    /// Settles every loop against the layers either side of it: whether
    /// anything stands on it, and how long its own column has stood.
    ///
    /// A part is closed partway up wherever a shoulder, a shelf, a counterbore
    /// or a screw-head recess ends one column of wall while the rest carries
    /// on. A bead left raised under one of those is buried by a surface
    /// metered for a full layer, which then lays about twice the material the
    /// gap can hold. The mirror is a column that begins partway up — the
    /// underside of a shelf, the roof of a bridged hole — whose first bead has
    /// no seam under it, so raising it by the full offset asks it to span a
    /// layer and a half of gap the slicer metered for one and leaves a void.
    /// Measured over three real slices, 2.4% to 2.9% of internal perimeter
    /// path is laid where nothing stands beneath it.
    ///
    /// Both answers come from the same walk of the loop's path, since the walk
    /// is what costs: three sets are tested for the price of one.
    fn mark_columns(&mut self) {
        // The object's last wall layer is capped whether or not the file gave
        // the survey the geometry to work the rest out for itself.
        let tops = self.object_tops.contains(&self.layer);
        let object = self.steps();
        let cells = |sets: &'a [Cells], layer: usize| sets.get(layer).filter(|c| !c.is_empty());
        let above = cells(self.uncovered, self.layer);
        let here = cells(self.unsupported, self.layer);
        // Two layers back is as far as the arithmetic looks: a column older
        // than the ramp takes the same offset however old it is.
        let below = self
            .layer
            .checked_sub(1)
            .and_then(|layer| cells(self.unsupported, layer));

        for index in 0..self.loops.len() {
            let (share, points) = self.shares([above, here, below], self.loops[index]);
            let over = |set: usize| points > 0 && share[set] as f64 > points as f64 * CAP_SHARE;
            self.loops[index].capped = tops || over(0);
            self.loops[index].steps = match (over(1), over(2)) {
                (true, _) => 0,
                (_, true) => 1,
                _ => object,
            };
        }
    }

    /// How much of a loop's path falls in each of the given sets, and how much
    /// path there was.
    fn shares(&self, sets: [Option<&Cells>; 3], current: Loop) -> ([usize; 3], usize) {
        let mut found = [0usize; 3];
        let mut walked = 0usize;
        // A loop starts where the one before it finished, and the first loop
        // of a region starts where the nozzle stood when the region opened.
        let mut from = match current.lead {
            0 => self.entry,
            lead => self.buffer[lead - 1].at,
        };
        for index in current.lead..current.end {
            let buffered = self.buffer[index];
            if buffered.extrudes {
                footprint::cells(from, buffered.at, buffered.arc, |cell| {
                    walked += 1;
                    for (at, set) in sets.iter().enumerate() {
                        found[at] += usize::from(set.is_some_and(|cells| cells.has(cell)));
                    }
                });
            }
            from = buffered.at;
        }
        (found, walked)
    }

    /// Books a raised loop's filament, and the share of it the multiplier
    /// added, so `--verbose` can price the setting against the whole part.
    fn meter(&mut self, body: usize, end: usize, factor: f64, steps: usize, capped: bool) {
        let stock: f64 = self.buffer[body..end]
            .iter()
            .filter_map(|buffered| buffered.delta)
            .filter(|delta| *delta > 0.0)
            .sum();
        self.raised_filament += stock * factor;
        // The multiplier's share is the factor less the geometry it scaled, so
        // a layer that changed height does not book its own flow as the cost
        // of the setting.
        self.multiplier_filament += stock * (factor - self.span(steps, capped));
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

    /// A file whose middle layer carries `body`, so neither the layers a
    /// column climbs over nor the one that caps it applies.
    ///
    /// The same wall runs the whole height of the file, as it does in a real
    /// one. Without a copy above, the body would be the last layer holding a
    /// wall, which caps it; without copies below, it would be a column that
    /// begins out of nowhere, which starts the climb instead. Either way the
    /// body measures something other than the steady state. The copies are
    /// stripped of their tags so they stay out of [`loop_states`], which does
    /// mean a body's loops are counted five times in `Stats::loops`.
    fn middle_layer(body: &str) -> String {
        let same = untagged(body);
        relative(&format!(
            "{}{same}{}{same}{}{same}{}{body}{}{same}",
            layer(0.2),
            layer(0.4),
            layer(0.6),
            layer(0.8),
            layer(1.0),
        ))
    }

    /// `body` with its trailing comments removed, so the same geometry can be
    /// emitted twice without the tags being counted twice. Marker lines, which
    /// are nothing but a comment, are kept whole.
    fn untagged(body: &str) -> String {
        let mut text = String::new();
        for line in body.lines() {
            let kept = match line.trim_start().starts_with(';') {
                true => line,
                false => line.split(';').next().unwrap_or(line).trim_end(),
            };
            text.push_str(kept);
            text.push('\n');
        }
        text
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
            out.contains("G1 X0.00 Y0.00 F9000 Z0.900 ; bricklayers brick raised"),
            "{out}"
        );
        assert!(
            out.contains("G1 Z0.800 F600 ; bricklayers brick reset"),
            "{out}"
        );
    }

    #[test]
    fn a_height_change_rides_the_travel_that_reaches_the_loop() {
        // A `G1 Z` of its own names no other axis, so the planner stops the
        // toolhead to run it and the nozzle sits primed over the loop's start
        // point while the axis crawls. Every loop starts at the seam, so an
        // aligned seam stacks the ooze from all of them into one line.
        let source = middle_layer(&format!(";TYPE:Perimeter\n{}", wall(3, "loop")));
        let out = run(&source, &Config::default());
        let halts = |note: &str| {
            out.lines()
                .filter(|line| line.starts_with("G1 Z") && line.ends_with(note))
                .count()
        };
        let ridden = out
            .lines()
            .filter(|line| line.contains(" X") && line.contains(BRICK_STAMP))
            .count();
        assert!(ridden > 0, "no height change rode a travel:\n{out}");
        assert_eq!(
            halts("raised"),
            0,
            "every raise has the travel that reaches its loop to ride:\n{out}"
        );
        // Riding a travel must not move the bead: every loop is still laid at
        // the height it was laid at before.
        assert!(
            out.contains("G1 X0.00 Y0.00 F9000 Z0.900 ; bricklayers brick raised"),
            "{out}"
        );
        assert!(
            !out.contains("G1 Z0.900"),
            "the raise still stopped the toolhead:\n{out}"
        );
    }

    #[test]
    fn a_height_change_never_rides_a_z_hop_down() {
        // Pulling a hop down to printing height would drag the nozzle through
        // exactly what the slicer lifted it to clear. The restore rides the
        // first extrusion here, so the hop is the last move of the lead and is
        // the one a careless rewrite would land on.
        let source = middle_layer(
            ";TYPE:Perimeter\n\
             G1 X0.45 Y0.45 F9000\n\
             G1 X9.55 Y0.45 E0.5\n\
             G1 X9.55 Y9.55 E0.5\n\
             G1 X0.45 Y9.55 E0.5\n\
             G1 X0.45 Y0.45 E0.5\n\
             G1 X0 Y0 Z1.4 F9000\n\
             G1 X10 Y0 Z0.8 E0.5\n\
             G1 X10 Y10 E0.5\n\
             G1 X0 Y10 E0.5\n\
             G1 X0 Y0 E0.5\n",
        );
        let out = run(&source, &Config::default());
        assert!(
            out.contains("G1 X0 Y0 Z1.4 F9000\n"),
            "the hop was flattened onto the layer:\n{out}"
        );
    }

    #[test]
    fn inserted_z_moves_carry_a_feedrate_and_hand_the_print_speed_back() {
        // A bare `G1 Z` inherits whatever `F` came last, which after a travel
        // slews the Z axis at travel speed. `F` is modal, so the print speed
        // has to be restored before the loop resumes. The travel here already
        // carries a comment, which is where the stamp would have to go, so the
        // height cannot ride it and the fallback is what runs.
        let source = middle_layer(
            ";TYPE:Perimeter\n\
             G1 X0.45 Y0.45 F9000\n\
             G1 X9.55 Y0.45 E0.5\n\
             G1 X9.55 Y9.55 E0.5\n\
             G1 X0.45 Y9.55 E0.5\n\
             G1 X0.45 Y0.45 E0.5\n\
             G1 X0 Y0 F9000 ; travel\n\
             G1 F1800\n\
             G1 X10 Y0 E0.5\n\
             G1 X10 Y10 E0.5\n\
             G1 X0 Y10 E0.5\n\
             G1 X0 Y0 E0.5\n",
        );
        let out = run(&source, &Config::default());
        assert!(
            out.contains("G1 Z0.900 F600 ; bricklayers brick raised"),
            "{out}"
        );
        assert!(out.contains("G1 F1800 ; bricklayers brick resume"), "{out}");
    }

    #[test]
    fn inserted_z_moves_fall_back_when_the_file_never_moves_z_alone() {
        let source = relative(&format!(
            ";LAYER_CHANGE\n;TYPE:Perimeter\nG1 X0 Y0 Z0.2 F9000\n{}\
             ;LAYER_CHANGE\n;TYPE:Perimeter\nG1 X0 Y0 Z0.4 F9000\n{}\
             ;LAYER_CHANGE\n;TYPE:Perimeter\nG1 X0 Y0 Z0.6 F9000\n{}\
             ;LAYER_CHANGE\n;TYPE:Perimeter\nG1 X0 Y0 Z0.8 F9000\n{}\
             ;LAYER_CHANGE\n;TYPE:Solid infill\nG1 X0 Y0 Z1.0 F9000\nG1 X10 Y0 E0.5\n",
            wall(2, "loop"),
            wall(2, "second"),
            wall(2, "third"),
            wall(2, "above")
        ));
        let out = run(&source, &Config::default());
        // Nothing here moves Z on its own, so the closing reset — which has no
        // travel left to ride — falls back to the built-in feedrate.
        assert!(
            out.contains("G1 Z0.600 F720 ; bricklayers brick reset"),
            "{out}"
        );
    }

    #[test]
    fn a_wall_that_starts_partway_up_climbs_from_where_it_starts() {
        // The mirror of a wall that ends: a column beginning on solid infill —
        // the underside of a shelf, the roof of a bridged hole — has no seam
        // under its first bead. Raising that bead by the full offset asks it
        // to span a layer and a half of gap the slicer metered for one, which
        // leaves a void. It has to climb from where it begins, exactly as a
        // column standing on the bed does.
        let body = format!(";TYPE:Perimeter\n{}", wall(2, "loop"));
        let mut source = String::new();
        // Two layers of solid infill, so the wall above them is supported
        // material but stands on no column of its own.
        for z in [0.2, 0.4] {
            source.push_str(&layer(z));
            source.push_str(";TYPE:Solid infill\nG1 X0 Y0 F9000\nG1 X10 Y0 E0.5\n");
        }
        for z in [0.6, 0.8, 1.0, 1.2, 1.4] {
            source.push_str(&layer(z));
            source.push_str(&untagged(&body));
        }
        let out = run(&relative(&source), &Config::default());
        let raises: Vec<&str> = out
            .lines()
            .filter(|line| line.ends_with("raised"))
            .map(|line| &line[line.find(" Z").expect("a height") + 2..line.len() - 27])
            .collect();
        // Nothing on the layer the column starts at, then a quarter and a half
        // of the layer as it climbs, then the offset it keeps.
        assert_eq!(raises, ["0.850", "1.100", "1.300"], "{out}");
        // The climbing beads are metered for the ground they gained; the one
        // above them spans exactly its own layer.
        assert!(out.contains("E0.62500"), "a climbing bead: {out}");
        assert!(
            out.contains("G1 X10.00 Y0.00 E0.5"),
            "the bead that starts the column is left as sliced: {out}"
        );
    }

    #[test]
    fn a_wall_that_ends_partway_up_is_capped_while_its_neighbour_carries_on() {
        // A shoulder: one column of wall runs on to the top of the part and
        // another stops here, closed by a surface printed at the next plane.
        // That surface is metered for a whole layer, so a bead left raised
        // under it fills half the gap with twice the material. Measured on the
        // real slice this came from, 293.8 mm of a 399.0 mm top surface sat on
        // a bead 0.1 mm proud.
        let on = wall_of(2, "on", 0.0, 10.0, 0.5);
        let ends = wall_of(2, "end", 20.0, 10.0, 0.5);
        let both = untagged(&format!(";TYPE:Perimeter\n{on}{ends}"));
        let mut source = String::new();
        // Both columns run up from the bed, or the layer under test would be
        // where they begin rather than where one of them ends.
        for z in [0.2, 0.4, 0.6, 0.8] {
            source.push_str(&layer(z));
            source.push_str(&both);
        }
        source.push_str(&layer(1.0));
        source.push_str(&format!(";TYPE:Perimeter\n{on}{ends}"));
        source.push_str(&layer(1.2));
        source.push_str(&format!(";TYPE:Perimeter\n{}", untagged(&on)));
        source.push_str(";TYPE:Solid infill\nG1 X20 Y20 F9000\nG1 X30 Y20 E0.5\nG1 X30 Y30 E0.5\n");
        let source = relative(&source);
        let out = run(&source, &Config::default());
        assert_eq!(
            loop_states(&out),
            vec![
                ("on1".to_owned(), false),
                ("on2".to_owned(), true),
                ("end1".to_owned(), false),
                ("end2".to_owned(), false),
            ],
            "only the wall that stops is capped: {out}"
        );
        assert!(
            out.contains("E0.25000 ; end2"),
            "and it gives back the half layer its column took: {out}"
        );
    }

    #[test]
    fn the_top_layer_caps_the_wall_flat() {
        // Raising here would stand a bead half a layer proud of the top
        // surface beside it, and meter it for half a gap that is a whole one.
        let body = format!(";TYPE:Perimeter\n{}", wall(2, "loop"));
        let same = untagged(&body);
        let source = relative(&format!(
            "{}{same}{}{same}{}{same}{}{body}{}",
            layer(0.2),
            layer(0.4),
            layer(0.6),
            layer(0.8),
            ";TYPE:Top solid infill\nG1 X40 Y0 E0.5\n"
        ));
        let out = run(&source, &Config::default());
        assert_eq!(
            loop_states(&out),
            vec![("loop1".to_owned(), false), ("loop2".to_owned(), false)],
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
        let body = format!(";TYPE:Perimeter\n{}", wall(2, "loop"));
        let same = untagged(&body);
        let source = relative(&format!(
            "{}{same}{}{same}{}{same}{}{body}{}{}",
            layer(0.2),
            layer(0.4),
            layer(0.6),
            layer(0.8),
            layer(1.0),
            ";TYPE:Top solid infill\nG1 X40 Y0 E0.5\n"
        ));
        let out = run(&source, &Config::default());
        assert_eq!(
            loop_states(&out),
            vec![("loop1".to_owned(), false), ("loop2".to_owned(), false)],
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
             ;LAYER_CHANGE\n;TYPE:Perimeter\nG1 X0 Y0 Z0.8 F9000\n{}\
             ;LAYER_CHANGE\n;TYPE:Solid infill\nG1 X0 Y0 Z1.0 F9000\nG1 X10 Y0 E0.5\n",
            wall(2, "first"),
            wall(2, "second"),
            wall(2, "third"),
            wall(2, "above"),
        ));
        let out = run(&source, &Config::default());
        assert!(
            out.contains("G1 X0.00 Y0.00 F9000 Z0.450 ; bricklayers brick raised"),
            "{out}"
        );
        assert!(
            out.contains("G1 X0.00 Y0.00 F9000 Z0.700 ; bricklayers brick raised"),
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
        // Four per layer, on each of the five layers the fixture repeats the
        // wall over so that this one stands on a column and carries one.
        assert_eq!(outcome.stats.loops, 20);
        assert_eq!(
            outcome.stats.raised, 6,
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
            .find("Z0.900 ; bricklayers brick raised")
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
        // The other four are the fixture's own wall on the layers around it.
        assert_eq!(outcome.stats.loops, 5);
        // The bed layer stays flat and the top one is capped, so the three in
        // between are raised.
        assert_eq!(outcome.stats.raised, 3);
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
        assert_eq!(outcome.stats.raised, 3);
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
        assert_eq!(outcome.stats.loops, 15);
        assert_eq!(outcome.stats.raised, 6);
    }

    #[test]
    fn restores_z_before_leaving_the_perimeter_region() {
        let source = middle_layer(&format!(
            ";TYPE:Perimeter\n{};TYPE:Solid infill\nG1 X40 Y0 E0.5\n",
            wall(2, "loop")
        ));
        let out = run(&source, &Config::default());
        // The fixture repeats the wall on every layer, so anchor on the
        // tagged one rather than on the file's first solid infill.
        let tail = &out[out.find("; loop2").expect("tagged loop kept")..];
        let reset = tail.find("bricklayers brick reset").expect("reset emitted");
        let infill = tail.find(";TYPE:Solid infill").expect("marker kept");
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

    /// The half layer a column is displaced by is paid over two layers rather
    /// than in one bead, and given back in one when the column is capped.
    #[test]
    fn a_column_climbs_to_its_offset_instead_of_jumping() {
        let mut source = String::from("; layer_height = 0.2\nM83\n");
        for index in 0..5 {
            source.push_str(&layer(0.2 + f64::from(index) * 0.2));
            source.push_str(";TYPE:Perimeter\n");
            source.push_str(&wall_of(2, &format!("L{index}loop"), 0.0, 10.0, 1.0));
        }
        let out = run(&source, &Config::default());
        let flow = |tag: &str| {
            out.lines()
                .find(|line| line.ends_with(tag))
                .map(|line| Line::parse(line).e.expect("an extrusion"))
                .unwrap_or_else(|| panic!("{tag} missing from:\n{out}"))
        };
        // Flat on the bed, a quarter of a layer taller on each of the two
        // climbing layers, as sliced once the column is up, and half a layer
        // short where the cap gives the climb back.
        assert_eq!(flow("L0loop2"), 1.0, "bed layer");
        assert_eq!(flow("L1loop2"), 1.25, "first climb");
        assert_eq!(flow("L2loop2"), 1.25, "second climb");
        assert_eq!(flow("L3loop2"), 1.0, "column up");
        assert_eq!(flow("L4loop2"), 0.5, "cap");
        assert!(
            out.contains("Z0.450 ; bricklayers brick raised"),
            "half the shift on the first climb:\n{out}"
        );
        assert!(
            out.contains("Z0.700 ; bricklayers brick raised"),
            "full shift on the second:\n{out}"
        );
    }

    /// A bead on the bed is pressed against the build plate rather than
    /// against a layer, so raising it presses nothing and the extra flow it
    /// would need spreads sideways. There is no seam under it to stagger
    /// either. On a Benchy this filled in the bottom nameplate, which is one
    /// layer deep.
    #[test]
    fn the_layer_laid_on_the_bed_is_never_raised() {
        let mut source = String::from("; layer_height = 0.2\nM83\n");
        for index in 0..4 {
            source.push_str(&layer(0.2 + f64::from(index) * 0.2));
            source.push_str(";TYPE:Perimeter\n");
            source.push_str(&wall_of(2, &format!("L{index}loop"), 0.0, 10.0, 1.0));
        }
        let outcome = apply(&source, &Config::default());
        let bed: Vec<&str> = outcome
            .gcode
            .lines()
            .take_while(|line| !line.contains("L1loop"))
            .collect();
        assert!(
            !bed.iter().any(|line| line.contains("raised")),
            "nothing may be raised on the bed layer:\n{}",
            bed.join("\n")
        );
        assert!(
            bed.iter().all(|line| !line.contains("E1.5")),
            "nor over-extruded:\n{}",
            bed.join("\n")
        );
    }

    /// The cap gives back exactly what the column climbed, which is less than
    /// half a layer when the wall ended before it finished climbing.
    #[test]
    fn a_cap_gives_back_only_the_climb_the_column_took() {
        let mut source = String::from("; layer_height = 0.2\nM83\n");
        for index in 0..3 {
            source.push_str(&layer(0.2 + f64::from(index) * 0.2));
            source.push_str(";TYPE:Perimeter\n");
            source.push_str(&wall_of(2, &format!("L{index}loop"), 0.0, 10.0, 1.0));
        }
        let out = run(&source, &Config::default());
        let flow = |tag: &str| {
            out.lines()
                .find(|line| line.ends_with(tag))
                .map(|line| Line::parse(line).e.expect("an extrusion"))
                .unwrap_or_else(|| panic!("{tag} missing from:\n{out}"))
        };
        assert_eq!(flow("L1loop2"), 1.25, "climbed a quarter of a layer");
        assert_eq!(flow("L2loop2"), 0.75, "so the cap gives a quarter back");
    }

    /// A wall that stands for two layers never climbs, so it is left exactly
    /// as the slicer wrote it. Embossed text and other one- or two-layer
    /// detail lands here.
    #[test]
    fn a_wall_too_short_to_climb_is_left_alone() {
        let wall = format!(";TYPE:Perimeter\n{}", wall_of(2, "loop", 0.0, 10.0, 1.0));
        let source = relative(&format!("{}{wall}{}{wall}", layer(0.2), layer(0.4)));
        let out = run(&source, &Config::default());
        assert!(!out.contains("raised"), "{out}");
        assert!(!out.contains("E1.25000"), "{out}");
    }

    /// A file whose slicer varied the layer height, with `body` on a layer
    /// half as deep as the rest of them.
    ///
    /// The layer under test runs 0.6 to 0.7 while every other one is 0.2, so a
    /// raise taken from its own height cannot be confused with one taken from
    /// the 0.2 the file declares. It sits three layers above the bed, clear of
    /// the [`RAMP`], and carries a wall above it for the reason
    /// [`middle_layer`] does.
    fn varied_layers(body: &str) -> String {
        let same = untagged(body);
        let wall = ";TYPE:Perimeter\n\
             G1 X0 Y0 F9000\n\
             G1 X10 Y0 E0.5\n\
             G1 X10 Y10 E0.5\n\
             G1 X0 Y10 E0.5\n\
             G1 X0 Y0 E0.5\n";
        relative(&format!(
            "{}{wall}{}{same}{}{same}{}{body}{}{wall}",
            layer(0.2),
            layer(0.4),
            layer(0.6),
            layer(0.7),
            layer(0.9),
        ))
    }

    /// Half of one layer height for the whole file staggers every layer that
    /// is not that height by the wrong amount, and an adaptive slice has
    /// almost none that are. Measured on a real Benchy sliced adaptively: the
    /// layers ran 0.081 to 0.119 mm against a declared 0.2, so 383 of 511 were
    /// lifted further than their own height and stood clear of the layer above
    /// with a gap underneath.
    #[test]
    fn a_raise_is_half_of_the_layer_it_belongs_to() {
        let source = varied_layers(&format!(";TYPE:Perimeter\n{}", wall(2, "loop")));
        let out = run(&source, &Config::default());
        assert!(
            out.contains("Z0.750 ; bricklayers brick raised"),
            "the layer is 0.1 deep, so it takes 0.05:\n{out}"
        );
        assert!(
            !out.contains("Z0.800 ; bricklayers brick raised"),
            "half the declared 0.2 is a whole layer here:\n{out}"
        );
    }

    /// A raised bead starts on top of whatever its own column left on the
    /// layer below, so where the layer thins the column has already filled
    /// part of it and the bead is metered for the gap that is left.
    #[test]
    fn a_bead_is_metered_for_the_gap_its_own_column_left() {
        let source = varied_layers(&format!(";TYPE:Perimeter\n{}", wall(2, "loop")));
        let out = run(&source, &Config::default());
        let raised = out
            .lines()
            .find(|line| line.ends_with("loop2"))
            .map(|line| Line::parse(line).e.expect("an extrusion"))
            .unwrap_or_else(|| panic!("loop2 missing from:\n{out}"));
        // The column below stands 0.1 above the 0.6 plane and the nozzle is at
        // 0.75, so the bead spans 0.05 of a layer metered for 0.1.
        assert_eq!(raised, 0.25, "half the flow of a 0.5 bead:\n{out}");
    }

    /// A height given on the command line is the one the caller wants used, so
    /// it stands in for the measurement rather than beside it.
    #[test]
    fn a_given_layer_height_overrides_what_the_layers_measure() {
        let source = varied_layers(&format!(";TYPE:Perimeter\n{}", wall(2, "loop")));
        let config = Config {
            layer_height: Some(0.2),
            ..Config::default()
        };
        let out = run(&source, &config);
        assert!(out.contains("Z0.800 ; bricklayers brick raised"), "{out}");
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
        let body = ";TYPE:Perimeter\n\
             G1 X0.45 Y0.45 F9000\n\
             G1 X9.55 Y0.45 E{a} ; loop1\n\
             G1 X9.55 Y9.55 E{b}\n\
             G1 X0.45 Y9.55 E{c}\n\
             G1 X0.45 Y0.45 E{d}\n\
             G1 X0 Y0 F9000\n\
             G1 X10 Y0 E{e} ; loop2\n\
             G1 X10 Y10 E{f}\n\
             G1 X0 Y10 E{g}\n\
             G1 X0 Y0 E{h}\n";
        // One absolute stream climbing by 1 mm a move, over a wall that runs
        // the height of the file so the layer under test is neither where its
        // column starts nor where it ends.
        let mut source = String::from("; layer_height = 0.2\nM82\n");
        let mut e = 0.0;
        let next = |e: &mut f64| {
            let mut text = body.to_string();
            for key in ["{a}", "{b}", "{c}", "{d}", "{e}", "{f}", "{g}", "{h}"] {
                *e += 1.0;
                text = text.replacen(key, &format!("{e:.1}"), 1);
            }
            text
        };
        // Only the layer under test keeps its tags, so the copies that give
        // its column something to stand on cannot be matched instead.
        for z in [0.2, 0.4, 0.6, 0.8] {
            source.push_str(&layer(z));
            source.push_str(&untagged(&next(&mut e)));
        }
        source.push_str(&layer(1.0));
        source.push_str(&next(&mut e));
        e += 1.0;
        source.push_str(&format!(";TYPE:Solid infill\nG1 X30 Y0 E{e:.1}\n"));
        source.push_str(&layer(1.2));
        source.push_str(&untagged(&next(&mut e)));

        let config = Config {
            extrusion_multiplier: 2.0,
            ..Config::default()
        };
        let out = run(&source, &config);

        // Read the stream back as per-move deltas, which is what the machine
        // acts on and what has to stay right however much the rescale shifted
        // the absolute values.
        let mut last = 0.0;
        let mut moves = Vec::new();
        for line in out.lines() {
            let parsed = Line::parse(line);
            if !parsed.draws() {
                continue;
            }
            if let Some(value) = parsed.e {
                moves.push((line.to_owned(), value - last));
                last = value;
            }
        }
        assert!(
            moves.iter().all(|(_, delta)| *delta > 0.0),
            "the extruder never runs backwards: {out}"
        );
        let delta = |tag: &str| {
            moves
                .iter()
                .find(|(line, _)| line.ends_with(tag))
                .map(|(_, delta)| *delta)
                .unwrap_or_else(|| panic!("{tag} missing from:\n{out}"))
        };
        assert_eq!(delta("; loop1"), 1.0, "the loop on the plane is untouched");
        assert_eq!(delta("; loop2"), 2.0, "the raised loop is doubled");
        assert!(
            out.contains("G1 X30 Y0 E"),
            "the infill after it is kept: {out}"
        );
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

        // A column climbs over the two layers above the bed and gives the
        // climb back where it is capped, and it does that once per object
        // rather than once per file.
        let flow = |tag: &str| {
            out.lines()
                .find(|line| line.ends_with(tag))
                .map(|line| Line::parse(line).e.expect("an extrusion"))
                .unwrap_or_else(|| panic!("{tag} missing from:\n{out}"))
        };
        for object in 1..=2 {
            assert_eq!(
                flow(&format!("o{object}L0loop2")),
                0.5,
                "object {object} bed layer"
            );
            assert_eq!(
                flow(&format!("o{object}L1loop2")),
                0.625,
                "object {object} first climb"
            );
            assert_eq!(
                flow(&format!("o{object}L2loop2")),
                0.625,
                "object {object} second climb"
            );
            assert_eq!(
                flow(&format!("o{object}L3loop2")),
                0.25,
                "object {object} top layer"
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
        assert_eq!(stats.loops, 10);
        assert_eq!(stats.raised, 3);
        assert_eq!(stats.layers, 5);
        assert_eq!(stats.layer_height, 0.2);
        assert!(stats.layer_height_detected);
    }
}
