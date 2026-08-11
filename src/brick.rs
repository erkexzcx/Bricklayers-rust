//! Brick layering.
//!
//! Inside every perimeter region the loops are numbered and every other one is
//! raised by half a layer height. Adjacent loops then bond across a staggered
//! seam instead of stacking their weak points on top of each other, the same
//! way courses of bricks are offset.
//!
//! One region covers an island's outer wall, the walls of every hole in it and
//! whatever fragments a thin wall broke into, so the numbering restarts at each
//! contour. Otherwise a contour that gained or lost a loop would invert the
//! stagger of every contour printed after it.
//!
//! The visible wall takes part: it is metered at the same flow as the loops
//! behind it, it anchors the alternation running through the whole stack, and
//! each closed loop of it is drawn inward by half the width it gains so its
//! commanded outer face lands where the slicer drew it. Only what is not a wall
//! is left alone — the top and bottom surfaces, the infill, and the whole of the
//! layer laid on the build plate.

use std::io::{self, BufRead, Write};

use crate::feature::{Feature, is_layer_marker};
use crate::footprint::{self, Arc, Cells};
use crate::gcode::{Code, Extruder, Line, Lines, write_e};
use crate::inset::Edge;
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
/// first, so that a wall's opening travel can carry its height.///
/// A real lead is a handful of lines — a travel, a hop restore, a prime and
/// the markers. The cap is what keeps the promise that nothing larger than one
/// region is ever buffered, whatever a file puts between two extrusions.
const TAIL: usize = 64;

/// How close two of a loop's vertices have to be to count as the same point,
/// in mm. G-code coordinates carry three decimals, so anything under a micron
/// is the same place written twice.
const COINCIDENT: f64 = 1e-6;

#[derive(Clone, Copy, Debug)]
pub struct Config {
    /// Layer height in mm, used for every layer. `None` takes each layer's own
    /// height from the file, which is the only right answer where the slicer
    /// varied it.
    pub layer_height: Option<f64>,
    /// Flow every wall bead is metered at, over what its own geometry asks
    /// for, or `None` to derive it from the file.
    ///
    /// It compensates for a wall being laid against a staggered seam rather
    /// than a flat plane, where the nozzle cannot press the corner between two
    /// beads closed. **Every** wall is laid against one, the visible wall
    /// included — its neighbour is raised like any other — so every wall is
    /// metered at it, and the visible one is then drawn in by half the width
    /// that gives it, which leaves its commanded outer face where the slicer
    /// drew it.
    /// Nothing that is not a wall is re-metered, and never the layer laid on
    /// the build plate — a bead there is pressed by the plate rather than by
    /// the layer under it, so surplus flow spreads sideways instead of filling
    /// anything.
    ///
    /// **The command line cannot pin one.** The flow follows each layer's own
    /// height, which on an adaptive slice changes every layer, so a constant
    /// would be wrong nearly everywhere; `--extra-flow` names the slope
    /// instead. This is here for tests and for a library caller that has
    /// measured its own.
    pub wall_flow: Option<f64>,
    /// Extra flow a wall takes when its layer is as thick as the nozzle, as a
    /// fraction. [`DEFAULT_EXTRA_FLOW`] by default, and never outside
    /// [`MIN_EXTRA_FLOW`] to [`MAX_EXTRA_FLOW`].
    ///
    /// A layer half the nozzle takes about half of it, a quarter takes a
    /// quarter, so it reads directly off a profile: at 0.05 a 0.2 mm layer
    /// through a 0.4 mm nozzle takes 2.5% over. It is only *about*, because
    /// what the flow actually follows is the line width the file states, not
    /// the nozzle — see [`automatic_flow`].
    ///
    /// It names the slope, not the answer, which is what keeps the per-layer
    /// derivation: an adaptive slice still meters every layer for its own
    /// height, just along a steeper or shallower line. Zero leaves every bead
    /// metered exactly as it was sliced and only the raise applied. The
    /// visible wall's inward move follows it, since that is half of whatever
    /// width the flow adds.
    pub extra_flow: f64,
    /// Width the internal perimeters were metered at, in mm, which sets the
    /// spacing the derived flow is read from. `None` falls back to the flow
    /// the reference profile takes.
    pub wall_width: Option<f64>,
    /// True when the slicer prints the external perimeter before the loops
    /// behind it, which decides which end of a wall the numbering starts from.
    /// Every mainstream slicer prints it last by default.
    pub external_perimeters_first: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            layer_height: None,
            wall_flow: None,
            extra_flow: DEFAULT_EXTRA_FLOW,
            wall_width: None,
            external_perimeters_first: false,
        }
    }
}

/// Extra flow a wall takes where its layer is as thick as the nozzle, as a
/// fraction.
///
/// Small on purpose: it is paid on every wall of the part, and it also decides
/// how far the visible wall is drawn in, which no one wants measured in
/// anything but microns. At the commonest profile of all — a 0.2 mm layer
/// through a 0.4 mm nozzle, half as thick as it is wide — it works out at
/// 2.5% over.
pub const DEFAULT_EXTRA_FLOW: f64 = 0.05;

/// What [`Config::extra_flow`] accepts, as a fraction.
///
/// Zero is the raise and nothing else, with every bead metered exactly as it
/// was sliced. The top of the range is ten times [`DEFAULT_EXTRA_FLOW`], which
/// is for sweeping a test print rather than for printing with.
pub const MIN_EXTRA_FLOW: f64 = 0.0;
pub const MAX_EXTRA_FLOW: f64 = 0.5;

/// The profile every measurement behind this tool was taken on: a 0.4 mm
/// nozzle laying a 0.45 mm internal wall at 0.2 mm layers.
const REFERENCE_NOZZLE: f64 = 0.4;
const REFERENCE_HEIGHT: f64 = 0.2;
const REFERENCE_WIDTH: f64 = 0.45;

/// Centre to centre distance a slicer lays neighbouring beads at, in mm.
///
/// A bead is a rectangle with a half-round cap at each side, so two of them
/// laid `width` apart would leave the corner between the caps empty. Slicers
/// close that by pulling them together until the overlap in the middle pays
/// for the corners, which is `width - height * (1 - pi/4)`. Measured on a real
/// OrcaSlicer file at 0.2 mm layers and 0.45 mm walls: neighbouring loops run
/// 0.4074 mm apart against the formula's 0.4071, and the file meters each bead
/// at 0.0773 mm2 against the formula's 0.0774 — the width alone would say
/// 0.0855.
fn bead_spacing(height: f64, width: f64) -> f64 {
    width - height * (1.0 - std::f64::consts::FRAC_PI_4)
}

/// Most flow a wall of this geometry can be metered at, as a multiple of what
/// it was sliced for.
///
/// A bead metered at `flow` is `flow * spacing + height * (1 - pi/4)` wide,
/// because its area is `flow` times the `height * spacing` the slicer meant
/// and its own round caps cost it the rest. The loop beside it is one spacing
/// away, so at twice the spacing this bead's edge lands on that loop's centre
/// — past there it is swallowing its neighbour rather than filling the corner
/// between them. Solving `flow * spacing + height * (1 - pi/4) = 2 * spacing`
/// gives this, so the limit is the bead model's own and not a number picked to
/// look safe.
///
/// It comes out under 1 only where the file states a width the slicer could
/// not have laid at that height — beads already past each other's centres — so
/// the caller floors it there and such a wall takes no extra at all.
fn flow_ceiling(height: f64, spacing: f64) -> f64 {
    2.0 - height * (1.0 - std::f64::consts::FRAC_PI_4) / spacing
}

/// Flow to meter a wall at, for a layer of `height` printed at `width`, where
/// a layer as thick as the nozzle would take `extra` over.
///
/// The corner the spacing above leaves between two beads is `height` tall and
/// closes as they are pushed together, so the share of a bead sitting in one
/// is proportional to `height / spacing` — a thick layer through a fine nozzle
/// has several times the junction a thin layer through a wide one has. Against
/// a flat plane the nozzle presses those corners closed on the way past; over
/// a staggered seam half of each is out of its reach, and that is what this
/// pays for.
///
/// `extra` sets the slope and the geometry sets where on it this layer sits.
/// The anchor is the reference profile, whose layer is half its nozzle, so
/// `extra` there gives half of itself. A file that states no width is metered
/// as if it were that profile.
pub fn automatic_flow(height: f64, width: Option<f64>, extra: f64) -> f64 {
    let extra = match extra.is_finite() {
        // A slope that is not a number would put NaN in an E word, and
        // `f64::clamp` passes one straight through.
        true => extra.clamp(MIN_EXTRA_FLOW, MAX_EXTRA_FLOW),
        false => DEFAULT_EXTRA_FLOW,
    };
    let at_reference = extra * REFERENCE_HEIGHT / REFERENCE_NOZZLE;
    let Some(width) = width.filter(is_a_height).filter(|_| is_a_height(&height)) else {
        return 1.0 + at_reference;
    };
    let spacing = bead_spacing(height, width);
    if !is_a_height(&spacing) {
        return 1.0 + at_reference;
    }
    let junction =
        (height / spacing) / (REFERENCE_HEIGHT / bead_spacing(REFERENCE_HEIGHT, REFERENCE_WIDTH));
    // Not `clamp`, which panics where the geometry puts the ceiling under 1.
    (1.0 + at_reference * junction)
        .min(flow_ceiling(height, spacing))
        .max(1.0)
}

impl Config {
    /// The flow this layer's walls are metered at.
    ///
    /// [`Config::wall_flow`] where a caller pinned one, and otherwise what the
    /// geometry asks for at the slope [`Config::extra_flow`] names.
    fn flow_at(&self, height: f64, width: Option<f64>) -> f64 {
        match self.wall_flow {
            Some(pinned) => pinned,
            None => automatic_flow(height, width, self.extra_flow),
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
    /// The part of `filament` that the flow multiplier added over the flow the
    /// geometry alone asks for.
    pub multiplier_filament: f64,
    /// Least and most flow any wall was metered at. The two differ only where
    /// the slicer varied the layer height.
    pub flow: Option<(f64, f64)>,
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
        flow: pass.flow,
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
    /// True where the line is a `G92`, whose `E` is an origin rather than a
    /// demand for filament and so reaches the output stream only when the
    /// line is written.
    resets_origin: bool,
}

/// Where a buffered move has to end up instead, and what its bead's length
/// changed by getting there, so flow per mm stays what the slicer metered.
#[derive(Clone, Copy)]
struct Moved {
    to: (f64, f64),
    /// Where the centre of an arc now sits relative to its start, which moved
    /// with the rest of the loop. `None` for a straight move.
    centre: Option<(f64, f64)>,
    ratio: f64,
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
    /// True where this loop is the visible wall, which anchors the numbering
    /// of the contour it belongs to and is the one loop that gets moved
    /// sideways.
    external: bool,
    /// True where any part of this loop was labelled a hidden wall. A loop
    /// that is neither is one the slicer only ever called an overhang, and
    /// nothing in the file says which wall that was.
    hidden: bool,
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
    /// Width the visible wall was metered at, in mm, which turns the flow it
    /// gains into the distance it has to be brought toward the loop behind it.
    /// Never absent: a wall that gains material without moving grows the part,
    /// so where the file states no width this falls back to the same profile
    /// the flow itself does.
    skin_width: f64,
    /// Width the hidden walls were metered at, in mm, which is what the flow
    /// is derived from. [`Config::wall_width`] where the caller knew better
    /// than the file — a binary container states it outside its G-code, and a
    /// slicer running this as a post-processing script exports it — and
    /// otherwise whatever the file itself said.
    wall_width: Option<f64>,
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
    /// Least and most flow any wall was metered at, for reporting.
    flow: Option<(f64, f64)>,
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
            // The visible wall gains material whether or not the file says how
            // wide it is, and material it gains without being moved grows the
            // part. So the same profile the flow falls back to stands in here:
            // the hidden walls' width where only that is stated, and the
            // reference profile where nothing is. A width off by a tenth
            // misplaces the face by a fraction of a micron; not moving at all
            // misplaces it by the whole offset.
            skin_width: survey
                .skin_width
                .or(config.wall_width)
                .or(survey.wall_width)
                .unwrap_or(REFERENCE_WIDTH),
            wall_width: config.wall_width.or(survey.wall_width),
            travelled: false,
            loops_seen: 0,
            raised: 0,
            capped: 0,
            at: (0.0, 0.0),
            entry: (0.0, 0.0),
            raise: None,
            flow: None,
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
                // A wall's loops are grouped and numbered as one, so the two
                // regions a slicer splits it across stay in one buffer: the
                // visible wall takes its place in the same alternation as the
                // loops behind it.
                let continues = self.feature.is_perimeter() && feature.is_perimeter();
                if !self.loops.is_empty() && !continues {
                    self.flush()?;
                }
                self.feature = feature;
                if continues && !self.buffer.is_empty() {
                    self.buffer(raw, line);
                    return Ok(());
                }
                return self.keep(raw, line, self.at);
            }
        }

        match line.code {
            Code::AbsoluteE | Code::RelativeE => self.extruder.set_mode(line.code),
            Code::SetPosition => {
                // A `G92` redefines the extruder origin. Loops that are still
                // buffered may yet be reordered, so the reset has to be
                // metered out with them rather than jumping ahead of them.
                // A tail holds no loops and replays in the order it arrived,
                // so the reset can travel with it — flushing there would throw
                // away the move the next region's raise was going to ride, and
                // Cura writes a `G92 E0` at every layer change.
                if !self.loops.is_empty() {
                    self.flush()?;
                }
                if let Some(e) = line.e {
                    self.extruder.observe_origin(e);
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

        if self.feature.is_perimeter() {
            if self.buffer.is_empty() {
                self.entry = from;
            }
            self.buffer(raw, line);
            return Ok(());
        }

        self.keep(raw, line, from)
    }

    /// Where each buffered line should end up, and by how much its bead's
    /// length changed getting there, or `None` where it stays exactly where
    /// the slicer put it.
    ///
    /// Only the visible wall is ever moved. It gains material like every other
    /// wall, and a bead widens about its own centre, so left alone that
    /// material would push the surface outward. Moving the loop inward by half
    /// the width it gains sends the gain into the joint behind it instead and
    /// leaves the commanded outer face exactly where the slicer drew it.
    ///
    /// An arc moves with the rest: its centre stays put and its radius changes
    /// by the offset, which is what the vertices either end of it are pulled
    /// onto. Four things it declines, passing the loop through as sliced: an
    /// open fragment, which has no inside; fewer than three points; a loop
    /// whose arcs could not be moved without distorting the circle they were
    /// drawn on; and the whole of the layer laid on the build plate, where
    /// there is no staggered joint to close.
    fn move_walls(&self) -> Vec<Option<Moved>> {
        let mut moved = vec![None; self.buffer.len()];
        let inward = self.skin_offset();
        let width = self.skin_width;
        if inward <= 0.0 || self.steps() == 0 {
            return moved;
        }
        let span = |from: (f64, f64), to: (f64, f64)| (to.0 - from.0).hypot(to.1 - from.1);

        for current in self.loops.iter().filter(|current| current.external) {
            let beads: Vec<usize> = (current.body..current.end)
                .filter(|&at| self.buffer[at].extrudes)
                .collect();
            if beads.len() < 3 {
                continue;
            }
            let entry = match current.body {
                0 => self.entry,
                body => self.buffer[body - 1].at,
            };
            // A ring does NOT return exactly to where it started: slicers stop
            // a bead short of its own seam so the two ends do not pile up.
            // Measured over 308 loops of two real OrcaSlicer files, every one
            // of them lands 0.0385 to 0.0411 mm short — the `seam_gap`
            // default, a tenth of a 0.4 mm nozzle — and none lands anywhere
            // else. A whole bead width is ten times that and still far under
            // anything an open fragment leaves, so it separates the two.
            let closes = self.buffer[beads[beads.len() - 1]].at;
            if span(closes, entry) >= width {
                continue;
            }

            // Every vertex the loop was drawn through, the closing one
            // included, so the seam gap survives being offset instead of being
            // pulled shut. A loop that does close exactly would hand the
            // offset the same point twice, which names no direction.
            let mut ring = vec![entry];
            ring.extend(beads.iter().map(|&at| self.buffer[at].at));
            let seamed = span(ring[ring.len() - 1], ring[0]) > COINCIDENT;
            if !seamed {
                ring.pop();
            }
            // How the loop travels out of each of those vertices. An arc
            // states its centre relative to where it starts, which is the
            // vertex before it.
            let mut edges: Vec<Edge> = beads
                .iter()
                .enumerate()
                .map(|(step, &at)| match self.buffer[at].arc {
                    Some(arc) => Edge::Arc {
                        centre: (ring[step].0 + arc.i, ring[step].1 + arc.j),
                        clockwise: arc.clockwise,
                    },
                    None => Edge::Straight,
                })
                .collect();
            if seamed {
                edges.push(Edge::Straight);
            }

            let Some(mut offset) = crate::inset::offset(&ring, &edges, inward) else {
                continue;
            };
            if seamed {
                // The closing vertex sits on the same edge as the loop's start
                // and is offset along its own normal, while the start is a
                // corner and moves along two. Left alone the gap between them
                // loses the whole offset, and at a wide enough one the bead
                // would run past its own seam and double up on it. Carrying the
                // start's move over keeps the gap the slicer chose, exactly.
                let last = offset.len() - 1;
                offset[last] = (
                    offset[0].0 + (closes.0 - entry.0),
                    offset[0].1 + (closes.1 - entry.1),
                );
                // Which can pull the last bead off the circle it is drawn on,
                // where that bead is an arc.
                if !crate::inset::keeps_its_arcs(&offset, &edges) {
                    continue;
                }
            }

            for (step, &at) in beads.iter().enumerate() {
                let next = (step + 1) % offset.len();
                let was = crate::inset::length(ring[step], ring[next], edges[step]);
                let now = crate::inset::length(offset[step], offset[next], edges[step]);
                let ratio = if was > 0.0 { now / was } else { 1.0 };
                // An arc names its centre from wherever it starts, so moving
                // its start moves the words that point at the centre too.
                let centre = match edges[step] {
                    Edge::Arc { centre, .. } => {
                        Some((centre.0 - offset[step].0, centre.1 - offset[step].1))
                    }
                    Edge::Straight => None,
                };
                moved[at] = Some(Moved {
                    to: offset[next],
                    centre,
                    ratio,
                });
            }
            // The travel that reached the loop has to land where it now
            // starts, or its first bead is laid from the old corner.
            if let Some(travel) = (current.lead..current.body)
                .rev()
                .find(|&at| self.buffer[at].positions)
            {
                moved[travel] = Some(Moved {
                    to: offset[0],
                    centre: None,
                    ratio: 1.0,
                });
            }
        }
        moved
    }

    /// Buffers a line that a region opening after it might still need, or
    /// writes it straight out.
    ///
    /// The travel that reaches a region's first loop is emitted before the
    /// `; FEATURE:` marker that opens the region, so without holding it back
    /// the first loop has nothing to carry its height and needs a `G1 Z` of
    /// its own — which stops the toolhead on the loop's start point, primed,
    /// which is the seam. Anything that lays no bead is held, because anything
    /// that lays no bead can sit between one region's last bead and the next
    /// region's first — a slicer drops progress, fan, acceleration and tool
    /// codes there freely, and ending the tail on one of those throws away the
    /// move the raise was going to ride. Holding only travels, height moves
    /// and comments lost the carrier for 2 of 132 raises on a stock
    /// OrcaSlicer file, and for all 132 once an `M73` followed every layer's
    /// `G1 Z`.
    fn keep(&mut self, raw: &str, line: Line<'_>, from: (f64, f64)) -> io::Result<()> {
        let lays = (line.x.is_some() || line.y.is_some()) && line.e.is_some();
        let holds = self.loops.is_empty() && self.buffer.len() < TAIL && !lays;
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
            resets_origin: line.code == Code::SetPosition,
        });

        if extrudes {
            if self.loops.is_empty() || self.travelled {
                self.open_loop(index);
            } else {
                // A slicer relabels a wall where it runs out over air, so one
                // loop can carry `Inner wall`, `Overhang wall` and `Outer
                // wall` in turn with no travel between. Which wall it is, is
                // whatever any bead of it was labelled.
                let feature = self.feature;
                if let Some(current) = self.loops.last_mut() {
                    current.external |= feature == Feature::ExternalPerimeter;
                    current.hidden |= feature == Feature::InternalPerimeter;
                }
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
            external: self.feature == Feature::ExternalPerimeter,
            hidden: self.feature == Feature::InternalPerimeter,
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
        let moved = self.move_walls();

        let head = self.loops.first().map_or(self.buffer.len(), |l| l.lead);
        for index in 0..head {
            self.replay(index, 1.0, &moved)?;
        }

        let mut last_raised = false;
        for index in 0..self.loops.len() {
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
                    Some(at_) if at_ == at => self.ride(at, target, raise, &moved)?,
                    _ => self.replay(at, 1.0, &moved)?,
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
            }
            // A loop metered exactly as sliced has nothing to book, and
            // summing its stock is the one cost here worth avoiding.
            if raise || factor != 1.0 {
                let geometry = self.geometry(current.raised, current.steps, current.capped);
                self.meter(current.body, end, factor, geometry, raise);
            }
            // The nozzle has to come back down before whatever the slicer
            // prints next, and the travel that leaves the region can carry it
            // just as well as the lead carried the way up.
            let closing = (index + 1 == self.loops.len() && raise)
                .then(|| self.carrier(current.body, end, self.layer_z))
                .flatten();
            for at in current.body..end {
                match closing {
                    Some(at_) if at_ == at => self.ride(at, self.layer_z, false, &moved)?,
                    _ => self.replay(at, factor, &moved)?,
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

        // A loop joins the contour it runs beside, which is not always the one
        // printed just before it: `inner-outer-inner` puts the visible wall
        // between the wall's two halves, so the loop after it is the innermost
        // one and can be the whole stack away. Comparing against every loop of
        // the open contour costs nothing extra on a wall of two or three and
        // keeps a thick one whole.
        //
        // One wall shows one visible loop, so a second one is a second wall
        // however close it runs. Without that, a Benchy's islands chain
        // together as each joined loop widens the contour's reach: measured at
        // 2 walls, 61 contours held two walls, one held nine, and the loops of
        // the second wall in each were numbered from the first wall's anchor.
        let mut contour = 0;
        let mut opened = 0;
        for index in 0..self.loops.len() {
            let taken =
                self.loops[index].external && (opened..index).any(|at| self.loops[at].external);
            let joins =
                index > 0 && !taken && (opened..index).rev().any(|at| self.adjacent(at, index));
            if !joins {
                contour += 1;
                opened = index;
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
    /// The visible wall is the loop that stays, so it is the anchor and it
    /// takes phase zero, which is flat. The alternation then runs inward
    /// through the whole stack, visible wall included: three loops leave both
    /// ends flat and raise the one between them, four raise the far end. A
    /// wall exposed on both faces therefore has one of its faces raised
    /// whenever the count is even, which is the point — nothing is held back
    /// from the stagger.
    ///
    /// A contour with no visible wall in it, which is what a hole's loops look
    /// like when the slicer split them across regions, falls back to numbering
    /// from the end the visible wall would have been at.
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
            let anchor = (start..end).find(|&at| self.loops[at].external);
            // Where the visible wall was printed at one end of the region, the
            // slicer worked through the stack in order and a loop's place in
            // it is its distance along the buffer. `inner-outer-inner` breaks
            // that: it prints the innermost wall last, right after the visible
            // one, which lands it a step from the anchor when it is a whole
            // stack away. Measuring the geometry is the only way to tell, and
            // it is only paid for where the order is not already monotonic.
            let ranked = anchor.filter(|&at| at != start && at + 1 != end).map(|at| {
                let mut gaps: Vec<(usize, f64)> = (start..end)
                    .map(|loop_| (loop_, self.gap(loop_, at)))
                    .collect();
                gaps.sort_by(|a, b| a.1.total_cmp(&b.1));
                gaps
            });

            if let Some(gaps) = ranked {
                for (rank, (at, _)) in gaps.into_iter().enumerate() {
                    self.loops[at].raised = !rank.is_multiple_of(2);
                }
                self.hold_overhangs(start, end);
                start = end;
                continue;
            }
            for offset in 0..loops {
                let phase = match anchor {
                    Some(at) => (start + offset).abs_diff(at),
                    None if self.config.external_perimeters_first => offset + 1,
                    None => loops - offset,
                };
                self.loops[start + offset].raised = !phase.is_multiple_of(2);
            }
            self.hold_overhangs(start, end);
            start = end;
        }
    }

    /// Leaves flat any loop the slicer only ever called an overhang.
    ///
    /// Nothing in the file says which wall such a loop came out of, and
    /// measured against ground truth (a slice with overhang detection off)
    /// **83.7% of overhang extrusion was really the visible wall**. Raising it
    /// on that evidence would put a step on the surface five times out of six,
    /// which is the one defect this exists to avoid. It carries 0.08% of a
    /// print, so holding it flat costs almost nothing.
    fn hold_overhangs(&mut self, start: usize, end: usize) {
        for current in &mut self.loops[start..end] {
            if !current.external && !current.hidden {
                current.raised = false;
            }
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

    /// The closest the two loops' paths come to each other, squared.
    ///
    /// Walls run parallel an extrusion width apart, so this puts a contour's
    /// loops in geometric order however the slicer sequenced them.
    fn gap(&self, loop_: usize, from: usize) -> f64 {
        let (loop_, from) = (self.loops[loop_], self.loops[from]);
        let stride = loop_.points.div_ceil(PROBES).max(1);
        self.points(loop_.body, loop_.end)
            .step_by(stride)
            .map(|(x, y)| {
                self.points(from.body, from.end)
                    .map(|(px, py)| (px - x).powi(2) + (py - y).powi(2))
                    .fold(f64::INFINITY, f64::min)
            })
            .fold(f64::INFINITY, f64::min)
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
    /// `G1 Z` that would otherwise have been inserted after it. A move that is
    /// also being taken sideways carries both at once: dropping the height
    /// onto a line of its own instead would put a toolhead stop back on the
    /// seam of every loop the visible wall is drawn in on.
    fn ride(
        &mut self,
        index: usize,
        z: f64,
        raised: bool,
        moved: &[Option<Moved>],
    ) -> io::Result<()> {
        let buffered = self.buffer[index];
        self.nozzle_z = Some(z);
        if let Some(rate) = buffered.f {
            self.feedrate = Some(rate);
        }
        let note = if raised { "raised" } else { "reset" };
        let to = moved[index];
        let Self { arena, out, .. } = self;
        // No `E` word to rescale: `Buffered::carries` is only true without one.
        let line = Line::parse(&arena[buffered.start..buffered.end]);
        let written = match to {
            Some(moved) => line.write_moved(out, moved.to, moved.centre, None, Some(z))?,
            None => false,
        };
        if !written {
            line.write_z(out, z)?;
        }
        writeln!(out, " ; {BRICK_STAMP}{note}")
    }

    /// Replays a buffered line, its flow scaled by `factor` and, where the
    /// visible wall is being taken sideways, its `X` and `Y` moved to `to`.
    /// The ratio beside the target is what the loop's length changed by, so
    /// flow per mm is what the slicer metered times the factor.
    fn replay(&mut self, index: usize, factor: f64, moved: &[Option<Moved>]) -> io::Result<()> {
        let buffered = self.buffer[index];
        let to = moved[index];
        if let Some(z) = buffered.z {
            self.nozzle_z = Some(z);
        }
        if let Some(rate) = buffered.f {
            self.feedrate = Some(rate);
        }
        if let Some(value) = buffered.e.filter(|_| buffered.resets_origin) {
            self.extruder.advance_origin(value);
        }
        let factor = factor * to.map_or(1.0, |moved| moved.ratio);
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
        let value = buffered.delta.map(|delta| extruder.advance(delta * factor));

        if let Some(moved) = to {
            let line = Line::parse(raw);
            if line.write_moved(out, moved.to, moved.centre, value, None)? {
                return out.write_all(b"\n");
            }
        }

        let Some(value) = value else {
            return write_line(out, raw);
        };
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
        if line.code == Code::SetPosition {
            if let Some(value) = line.e {
                self.extruder.advance_origin(value);
            }
        }
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
        self.geometry(raised, steps, capped) * self.multiplier()
    }

    /// What the bead's own shape asks for, before the multiplier. A loop left
    /// on the plane spans its layer and is metered as sliced.
    fn geometry(&self, raised: bool, steps: usize, capped: bool) -> f64 {
        if raised {
            self.span(steps, capped)
        } else {
            1.0
        }
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

    /// The flow the walls of this layer take, except on the layer laid on the
    /// build plate: a bead there is pressed by the plate rather than by the
    /// layer under it, so surplus flow spreads sideways instead of filling
    /// anything.
    fn multiplier(&self) -> f64 {
        if self.steps() > 0 { self.flow() } else { 1.0 }
    }

    /// [`Config::wall_flow`], or what this layer's own geometry
    /// asks for where nothing pinned it.
    ///
    /// Read per layer rather than once per file, so a slice whose layers vary
    /// meters each of them for the seam it actually has.
    fn flow(&self) -> f64 {
        self.config.flow_at(self.height(), self.wall_width)
    }

    /// How far the visible wall is brought toward the loop behind it, in mm.
    ///
    /// A bead widens about its own centre, so half of the width it gains goes
    /// outward; moving it in by that much leaves its commanded outer face
    /// exactly where the slicer drew it and sends the gain into the joint
    /// behind it. What it
    /// gains is `(flow - 1)` of its *spacing*, not of its nominal width: a
    /// bead of width `W` carries `h(W - h(1 - pi/4))`, so scaling that area by
    /// `flow` at the same height widens it by `(flow - 1)` spacings and the
    /// round caps cost the same either way. Zero where the file states no
    /// width to derive it from, or where the flow asks for no extra material
    /// in the first place.
    fn skin_offset(&self) -> f64 {
        (self.flow() - 1.0) / 2.0 * bead_spacing(self.height(), self.skin_width)
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

    /// Books what a loop's flow costs: the stock a raised loop lays, and the
    /// share of any loop's that the multiplier added, so `--verbose` can price
    /// the setting against the whole part.
    fn meter(&mut self, body: usize, end: usize, factor: f64, geometry: f64, raised: bool) {
        let stock: f64 = self.buffer[body..end]
            .iter()
            .filter_map(|buffered| buffered.delta)
            .filter(|delta| *delta > 0.0)
            .sum();
        if raised {
            self.raised_filament += stock * factor;
        }
        // The layer on the plate is metered as sliced, so reporting its flow
        // would put a 1.0 in every file's range that no wall was printed at.
        if stock > 0.0 && self.steps() > 0 {
            let flow = self.flow();
            self.flow = Some(match self.flow {
                Some((low, high)) => (low.min(flow), high.max(flow)),
                None => (flow, flow),
            });
        }
        // The multiplier's share is the factor less the geometry it scaled, so
        // a layer that changed height does not book its own flow as the cost
        // of the setting.
        self.multiplier_filament += stock * (factor - geometry);
    }

    fn move_z(&mut self, z: f64, raised: bool) -> io::Result<()> {
        if self.nozzle_z.is_some_and(|current| current == z) {
            return Ok(());
        }
        self.nozzle_z = Some(z);
        let note = if raised { "raised" } else { "reset" };
        let rate = self.z_feedrate;
        // Plain `Display`, not `{:.0}`: both rates were read off the file, and
        // rounding them to whole mm/min hands the print back a speed it never
        // asked for — `F0` for anything under half a unit.
        writeln!(self.out, "G1 Z{z:.3} F{rate} ; {BRICK_STAMP}{note}")?;
        match self.feedrate {
            Some(previous) if previous != rate => {
                writeln!(self.out, "G1 F{previous} ; {BRICK_STAMP}resume")
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

    /// The shipped settings with the flow left alone, for a test that measures
    /// what the geometry asks for rather than what the multiplier adds to it.
    fn plain() -> Config {
        Config {
            wall_flow: Some(1.0),
            ..Config::default()
        }
    }

    /// A file that states the width its visible wall was metered at, which is
    /// what turns a multiplier into a distance to draw that wall in by.
    ///
    /// 0.4 mm at a multiplier of 1.3 gives an offset of exactly 0.06, so the
    /// moved coordinates are readable rather than rounded.
    fn with_skin_width(body: &str) -> String {
        format!("; external_perimeter_extrusion_width = 0.4\n{body}")
    }

    fn drawn_in() -> Config {
        Config {
            wall_flow: Some(1.3),
            ..Config::default()
        }
    }

    /// A 10 mm square of visible wall, printed anticlockwise as a slicer emits
    /// an island's boundary.
    fn skin() -> String {
        format!(
            ";TYPE:External perimeter\n{}",
            wall_of(1, "skin", 0.0, 10.0, 1.0)
        )
    }

    /// The same square, stopped `gap` mm short of its own seam, which is what
    /// a slicer actually emits so the two ends of a ring do not pile up.
    fn seamed_skin(gap: f64) -> String {
        format!(
            ";TYPE:External perimeter\n\
             G1 X0.00 Y0.00 F9000\n\
             G1 X10.00 Y0.00 E1 ; skin1\n\
             G1 X10.00 Y10.00 E1\n\
             G1 X0.00 Y10.00 E1\n\
             G1 X0.00 Y{gap:.3} E1\n"
        )
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
    fn an_inserted_feedrate_hands_back_the_rate_the_file_asked_for() {
        // Rounding the restore to whole mm/min hands the print a speed it
        // never asked for, and anything under half a unit comes back as `F0`.
        let source = middle_layer(
            ";TYPE:Perimeter\n\
             G1 X0.45 Y0.45 F9000\n\
             G1 X9.55 Y0.45 E0.5\n\
             G1 X9.55 Y9.55 E0.5\n\
             G1 X0.45 Y9.55 E0.5\n\
             G1 X0.45 Y0.45 E0.5\n\
             G1 X0 Y0 F9000 ; travel\n\
             G1 F1799.5\n\
             G1 X10 Y0 E0.5\n\
             G1 X10 Y10 E0.5\n\
             G1 X0 Y10 E0.5\n\
             G1 X0 Y0 E0.5\n",
        );
        let out = run(&source, &Config::default());
        assert!(
            out.contains("G1 F1799.5 ; bricklayers brick resume"),
            "the restored feedrate was rounded:\n{out}"
        );
    }

    /// A slicer drops progress, fan, acceleration, tool and origin codes
    /// between the layer's `G1 Z` and the wall that follows it. Ending the
    /// held tail on one of those wrote the travel out before the raise could
    /// ride it, so the raise fell back to a `G1 Z` of its own — on the loop's
    /// start point, primed, which is the seam. Measured on a stock OrcaSlicer
    /// file it cost 2 of 132 raises, and 132 of 132 once an `M73` followed
    /// every layer's `G1 Z`.
    #[test]
    fn a_height_change_still_rides_a_travel_across_an_interrupting_command() {
        for interruption in ["M73 P1 R1", "M106 S255", "M204 S500", "T0", "G92 E0"] {
            // The loop's travel has to sit before the interruption, as it does
            // in a real file: the slicer reaches the wall, then declares the
            // region, and the first loop has no move of its own left.
            let loops = wall(3, "loop");
            let (travel, rest) = loops.split_once('\n').expect("a wall opens with a travel");
            let body = format!("{travel}\n{interruption}\n;TYPE:Perimeter\n{rest}");
            let same = untagged(&body);
            let source = relative(&format!(
                "{}{same}{}{same}{}{same}{}{body}{}{same}",
                layer(0.2),
                layer(0.4),
                layer(0.6),
                layer(0.8),
                layer(1.0),
            ));

            let out = run(&source, &Config::default());
            let halts = out
                .lines()
                .filter(|line| line.starts_with("G1 Z") && line.ends_with("raised"))
                .count();
            assert_eq!(halts, 0, "{interruption} cost a raise its carrier:\n{out}");
            assert!(
                out.contains("Z0.900 ; bricklayers brick raised"),
                "{interruption}: nothing was raised at all:\n{out}"
            );
            assert!(
                out.contains(&format!("\n{interruption}\n")),
                "{interruption} was dropped:\n{out}"
            );
        }
    }

    /// The same, in absolute mode, where a `G92` also has to keep the stream
    /// honest: the origin is read when the line is parsed but only reaches the
    /// output when the buffered tail is written, so the two halves move apart.
    #[test]
    fn a_g92_between_the_layer_and_the_wall_keeps_the_carrier_and_the_origin() {
        let loops = wall_of(3, "loop", 0.0, 10.0, 1.0);
        let (travel, rest) = loops.split_once('\n').expect("a wall opens with a travel");
        let body = format!("{travel}\nG92 E0\n;TYPE:Perimeter\n{rest}");
        let same = untagged(&body);
        let source = format!(
            "; layer_height = 0.2\nM82\n{}{same}{}{same}{}{same}{}{body}{}{same}",
            layer(0.2),
            layer(0.4),
            layer(0.6),
            layer(0.8),
            layer(1.0),
        );

        let out = run(&source, &Config::default());
        let halts = out
            .lines()
            .filter(|line| line.starts_with("G1 Z") && line.ends_with("raised"))
            .count();
        assert_eq!(halts, 0, "the reset cost the raise its carrier:\n{out}");

        // Every absolute value after the last reset is measured from the new
        // zero, so none of them may jump or run backwards.
        let after = out.rsplit_once("\nG92 E0\n").expect("the reset is kept").1;
        let mut position = 0.0;
        for line in after.lines() {
            let parsed = Line::parse(line);
            let Some(e) = parsed.e.filter(|_| parsed.draws()) else {
                continue;
            };
            assert!(
                e >= position && e - position <= 1.5,
                "{line} asks for {} mm in one move:\n{out}",
                e - position
            );
            position = e;
        }
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
        let out = run(&relative(&source), &plain());
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
        let out = run(&source, &plain());
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
        let out = run(&source, &plain());
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
        let out = run(&source, &plain());
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
        let out = run(&source, &Config::default());
        assert_eq!(
            loop_states(&out),
            vec![
                ("wall1".to_owned(), true),
                ("wall2".to_owned(), false),
                ("wall3".to_owned(), true),
                ("hole1".to_owned(), false),
                ("hole2".to_owned(), true),
            ],
            "{out}"
        );
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
            wall_flow: Some(2.0),
            ..Config::default()
        };
        let out = run(&source, &config);
        assert_eq!(
            loop_states(&out),
            vec![("inner".to_owned(), false), ("outer".to_owned(), true)],
            "the retraction must not split the wall: {out}"
        );
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
            wall_flow: Some(2.0),
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

    /// The visible wall is never raised, and a file that states no width for
    /// it has nothing to move it by, so it comes through exactly as sliced.
    #[test]
    fn external_perimeters_are_never_raised() {
        let source = middle_layer(";TYPE:External perimeter\nG1 X10 Y0 E0.5\nG1 X20 Y0 E0.5\n");
        assert!(!run(&source, &Config::default()).contains("bricklayers"));
    }

    /// The multiplier is a flow for the hidden walls, not compensation owed to
    /// the raise, so the loop left on the plane is scaled beside the one that
    /// was raised.
    #[test]
    fn every_internal_wall_is_metered_at_the_multiplier() {
        let source = middle_layer(&format!(
            ";TYPE:Perimeter\n{}",
            wall_of(2, "loop", 0.0, 10.0, 1.0)
        ));
        let config = Config {
            wall_flow: Some(1.5),
            ..Config::default()
        };
        let out = run(&source, &config);
        assert!(out.contains("E1.50000 ; loop2"), "raised loop:\n{out}");
        assert!(out.contains("E1.50000 ; loop1"), "flat loop:\n{out}");
        assert!(!out.contains("E1 ; loop"), "nothing left as sliced:\n{out}");
    }

    /// Everything the eye lands on that is not a wall is left alone: the solid
    /// surfaces that close the part top and bottom, and the infill between
    /// them, are not perimeters and are never rescaled.
    #[test]
    fn the_surfaces_that_show_are_left_as_sliced() {
        let source = middle_layer(&format!(
            ";TYPE:Top solid infill\nG1 X0 Y-2 F9000\nG1 X10 Y-2 E1.0 ; ceiling\n\
             ;TYPE:Internal infill\nG1 X0 Y-3 F9000\nG1 X10 Y-3 E1.0 ; fill\n\
             ;TYPE:Perimeter\n{}",
            wall_of(2, "loop", 0.0, 10.0, 1.0)
        ));
        let config = Config {
            wall_flow: Some(1.5),
            ..Config::default()
        };
        let out = run(&source, &config);
        for tag in ["ceiling", "fill"] {
            assert!(
                out.contains(&format!("E1.0 ; {tag}")),
                "{tag} moved:\n{out}"
            );
        }
        assert!(out.contains("E1.50000 ; loop1"), "wall scaled:\n{out}");
    }

    /// A bead on the plate is pressed by the plate rather than by a layer, so
    /// surplus flow there has nowhere to go but sideways.
    #[test]
    fn the_layer_on_the_bed_is_left_as_sliced() {
        let mut source = String::from("; layer_height = 0.2\nM83\n");
        for index in 0..5 {
            source.push_str(&layer(0.2 + f64::from(index) * 0.2));
            source.push_str(";TYPE:Perimeter\n");
            source.push_str(&wall_of(2, &format!("L{index}loop"), 0.0, 10.0, 1.0));
        }
        let config = Config {
            wall_flow: Some(1.5),
            ..Config::default()
        };
        let out = run(&source, &config);
        assert!(out.contains("E1 ; L0loop1"), "bed layer, flat loop:\n{out}");
        assert!(
            out.contains("E1 ; L0loop2"),
            "bed layer, raised loop:\n{out}"
        );
        assert!(
            out.contains("E1.50000 ; L3loop1"),
            "the layers above:\n{out}"
        );
    }

    /// A file handed no settings at all still gets the shipped slope, and the
    /// arithmetic of the raise is unchanged by it.
    #[test]
    fn the_shipped_default_meters_the_hidden_walls_over() {
        let source = middle_layer(&format!(
            ";TYPE:Perimeter\n{}",
            wall_of(2, "loop", 0.0, 10.0, 1.0)
        ));
        assert_eq!(Config::default().wall_flow, None);
        assert_eq!(Config::default().extra_flow, DEFAULT_EXTRA_FLOW);
        let out = run(&source, &Config::default());
        assert!(out.contains("E1.02500 ; loop1"), "{out}");
        assert!(out.contains("E1.02500 ; loop2"), "{out}");
    }

    /// The reference profile is the anchor: its layer is half its nozzle, so
    /// it takes half of whatever slope is set. A file that states no width is
    /// metered as if it were that profile.
    #[test]
    fn the_reference_profile_takes_half_the_slope() {
        let flow = |width| automatic_flow(0.2, width, DEFAULT_EXTRA_FLOW);
        assert_eq!(flow(Some(0.45)), 1.025);
        assert_eq!(flow(None), 1.025);
        assert_eq!(automatic_flow(0.2, Some(0.45), 0.10), 1.05);
        assert_eq!(automatic_flow(0.2, Some(0.45), 0.0), 1.0);
    }

    /// The corner two beads leave between them is as tall as the layer and as
    /// wide as what is left of the spacing, so the share of a bead sitting in
    /// one — and the flow that pays for it — rises with the layer height and
    /// falls as the wall widens.
    #[test]
    fn the_flow_follows_the_layer_height_against_the_wall_width() {
        let round = |value: f64| (value * 1000.0).round() / 1000.0;
        let flow = |height, width| automatic_flow(height, width, DEFAULT_EXTRA_FLOW);
        assert_eq!(round(flow(0.1, Some(0.45))), 1.012);
        assert_eq!(round(flow(0.28, Some(0.45))), 1.037);
        assert_eq!(round(flow(0.2, Some(0.35))), 1.033);
        assert_eq!(round(flow(0.2, Some(0.65))), 1.017);
        // A 0.8 mm nozzle at a fine layer has barely any junction to pay for.
        assert_eq!(round(flow(0.15, Some(0.85))), 1.009);
    }

    /// Every number reaching the nozzle is held to what a printer can act on,
    /// and a settings block is not a trustworthy source of any of them.
    #[test]
    fn a_width_a_bead_cannot_have_falls_back_to_the_shipped_flow() {
        for impossible in [
            Some(0.0),
            Some(-0.45),
            Some(f64::NAN),
            Some(f64::INFINITY),
            // Narrower than the caps the layer's own height gives the bead, so
            // the spacing works out at zero or less.
            Some(0.04),
            None,
        ] {
            assert_eq!(
                automatic_flow(0.2, impossible, DEFAULT_EXTRA_FLOW),
                1.025,
                "{impossible:?}"
            );
        }
        for impossible in [0.0, -0.2, f64::NAN] {
            assert_eq!(
                automatic_flow(impossible, Some(0.45), DEFAULT_EXTRA_FLOW),
                1.025,
                "{impossible}"
            );
        }
        // A slope that is not a number falls back to the shipped one, and
        // nothing derived may ask for more than a bead's neighbour can take.
        assert_eq!(automatic_flow(0.2, Some(0.45), f64::NAN), 1.025);
        assert_eq!(automatic_flow(0.2, Some(0.45), -1.0), 1.0);
        // A 0.3 mm layer through a 0.1 mm line is beads already laid past one
        // another's centres, so the ceiling floors out and it takes no extra.
        assert_eq!(automatic_flow(0.3, Some(0.1), 1e9), 1.0);
    }

    /// The ceiling is a guard against a width no slicer would state, not a
    /// setting anyone prints against, so pin that it stays out of the way over
    /// every geometry a slicer will produce: nozzles from 0.2 to 1.2 mm,
    /// widths out to 1.2× the nozzle, and layers from a tenth of it to four
    /// fifths. The bead only reaches its neighbour's centre past `h/s` of
    /// 1.38, and the widest layer in that sweep is four fifths of a nozzle
    /// narrower than its own line.
    #[test]
    fn the_flow_ceiling_never_binds_on_a_geometry_a_slicer_produces() {
        let (mut span, mut bound) = (0, 0);
        for nozzle in [0.2, 0.25, 0.3, 0.4, 0.5, 0.6, 0.8, 1.0, 1.2] {
            for wide in 0..=20 {
                let width = nozzle * (1.0 + f64::from(wide) / 100.0);
                for thick in 10..=80 {
                    let height = nozzle * f64::from(thick) / 100.0;
                    let ceiling = flow_ceiling(height, bead_spacing(height, width));
                    span += 1;
                    bound +=
                        usize::from(automatic_flow(height, Some(width), MAX_EXTRA_FLOW) >= ceiling);
                }
            }
        }
        assert_eq!(bound, 0, "{bound} of {span} reached the ceiling");
        // It is still a real limit. A 0.4 mm layer laid at a 0.37 mm line is
        // a bead nearly as tall as the gap beside it, and the top of the dial
        // is held back to what that gap can take.
        let (height, width) = (0.4, 0.37);
        assert_eq!(
            automatic_flow(height, Some(width), MAX_EXTRA_FLOW),
            flow_ceiling(height, bead_spacing(height, width))
        );
    }

    /// The width the file states is the one the flow is read from, so a
    /// profile that lays wider beads pays less for the same layer.
    #[test]
    fn the_width_the_file_states_sets_the_flow() {
        let walls = format!(";TYPE:Perimeter\n{}", wall_of(2, "loop", 0.0, 10.0, 1.0));
        let narrow = run(
            &format!("; inner_wall_line_width = 0.35\n{}", middle_layer(&walls)),
            &Config::default(),
        );
        let wide = run(
            &format!(
                "; perimeter_extrusion_width = 0.65\n{}",
                middle_layer(&walls)
            ),
            &Config::default(),
        );
        assert!(narrow.contains("E1.03314 ; loop1"), "{narrow}");
        assert!(wide.contains("E1.01676 ; loop1"), "{wide}");
    }

    /// The flow is read per layer, not once per file, so a slice whose layers
    /// vary meters each of them for the seam it actually has. A thick layer
    /// leaves more of each bead in the corner beside it than a thin one.
    #[test]
    fn an_adaptive_slice_meters_each_layer_at_its_own_flow() {
        let walls = |tag: &str| format!(";TYPE:Perimeter\n{}", wall_of(2, tag, 0.0, 10.0, 1.0));
        let mut source = String::from("; inner_wall_line_width = 0.45\nM83\n");
        // Heights of 0.1 and 0.3 either side of the reference, laid down as
        // planes so the survey measures them rather than reading a nominal.
        for (index, z) in [0.2, 0.3, 0.4, 0.7, 1.0, 1.1].into_iter().enumerate() {
            source.push_str(&layer(z));
            source.push_str(&walls(&format!("L{index}loop")));
        }
        let outcome = apply(&source, &Config::default());
        let out = outcome.gcode;

        assert!(outcome.stats.flow.is_some(), "{:?}", outcome.stats);
        let (low, high) = outcome.stats.flow.expect("walls were metered");
        let asks = |height| automatic_flow(height, Some(0.45), DEFAULT_EXTRA_FLOW);
        assert!(
            (low - asks(0.1)).abs() < 1e-9 && (high - asks(0.3)).abs() < 1e-9,
            "{low} to {high}"
        );
        // Layer 3 is 0.3 mm over a 0.4 mm plane, layer 1 is 0.1 mm over 0.2.
        assert!(out.contains("E1.03959 ; L3loop1"), "thick layer:\n{out}");
        assert!(out.contains("E1.01187 ; L1loop1"), "thin layer:\n{out}");
    }

    /// The dial names the slope, not the answer: it is the extra a wall takes
    /// where the layer is as thick as the nozzle, and the geometry decides
    /// where on that line each layer sits.
    #[test]
    fn the_dial_is_the_extra_a_layer_as_thick_as_the_nozzle_takes() {
        let source = format!(
            "; inner_wall_line_width = 0.45\n{}",
            middle_layer(&format!(
                ";TYPE:Perimeter\n{}",
                wall_of(2, "loop", 0.0, 10.0, 1.0)
            ))
        );
        let flow = |extra: f64| {
            let config = Config {
                extra_flow: extra,
                ..Config::default()
            };
            let out = run(&source, &config);
            let bead = out
                .lines()
                .find(|line| line.ends_with("; loop1"))
                .unwrap_or_else(|| panic!("a hidden wall:\n{out}"))
                .to_owned();
            Line::parse(&bead).e.expect("an E word")
        };
        // A 0.2 mm layer is half of the 0.4 mm nozzle, so it takes half.
        assert!((flow(0.05) - 1.025).abs() < 1e-9, "{}", flow(0.05));
        assert!((flow(0.10) - 1.05).abs() < 1e-9, "{}", flow(0.10));
        assert!((flow(0.02) - 1.01).abs() < 1e-9, "{}", flow(0.02));
        // Zero is the raise and nothing else: metered exactly as sliced.
        assert_eq!(flow(0.0), 1.0);
    }

    /// An adaptive slice still meters every layer for its own height whatever
    /// the slope is set to — the dial tilts the line, it does not replace it
    /// with a constant.
    #[test]
    fn the_dial_keeps_a_layer_metered_for_its_own_height() {
        let walls = |tag: &str| format!(";TYPE:Perimeter\n{}", wall_of(2, tag, 0.0, 10.0, 1.0));
        let mut source = String::from("; inner_wall_line_width = 0.45\nM83\n");
        for (index, z) in [0.2, 0.3, 0.4, 0.7, 1.0, 1.1].into_iter().enumerate() {
            source.push_str(&layer(z));
            source.push_str(&walls(&format!("L{index}loop")));
        }
        let config = Config {
            extra_flow: 0.025,
            ..Config::default()
        };
        let outcome = apply(&source, &config);
        let (low, high) = outcome.stats.flow.expect("walls were metered");
        let asks = |height| automatic_flow(height, Some(0.45), 0.025);
        assert!(
            (low - asks(0.1)).abs() < 1e-9 && (high - asks(0.3)).abs() < 1e-9,
            "{low} to {high}"
        );
        assert!(low < high, "the layers must still differ: {low} to {high}");
    }

    /// The visible wall is moved by half the width the flow adds, so turning
    /// the flow down moves it less. Scaling one without the other would grow
    /// or shrink the part.
    #[test]
    fn the_visible_wall_moves_with_the_dial() {
        let walls = format!(
            ";TYPE:Perimeter\n{}{}",
            wall_of(1, "loop", 0.6, 8.8, 1.0),
            skin()
        );
        let source = format!(
            "; external_perimeter_extrusion_width = 0.4\n; inner_wall_line_width = 0.45\n{}",
            middle_layer(&walls)
        );
        let inset = |extra: f64| {
            let config = Config {
                extra_flow: extra,
                ..Config::default()
            };
            let out = run(&source, &config);
            let bead = out
                .lines()
                .find(|line| line.ends_with("; skin1"))
                .unwrap_or_else(|| panic!("the visible wall:\n{out}"))
                .to_owned();
            Line::parse(&bead).y.expect("a Y")
        };
        // Half of (flow - 1) times the spacing the 0.4 mm wall is laid at,
        // 0.357 at these layers, on the three-decimal grid a coordinate is
        // written to: 0.0045 at the shipped slope, and half of that, 0.0022,
        // lands on 0.002.
        assert_eq!(inset(0.05), 0.004);
        assert_eq!(inset(0.025), 0.002);
        assert_eq!(inset(0.10), 0.009);
        // No extra flow is no extra width, so there is nothing to move.
        assert_eq!(inset(0.0), 0.0);
    }

    /// Nothing a library caller passes may reach the nozzle as a coordinate it
    /// cannot act on, and `f64::clamp` hands NaN straight back. A number past
    /// the range is pulled into it, and the flow ceiling holds whatever is
    /// left.
    #[test]
    fn a_slope_a_printer_cannot_act_on_is_ignored() {
        let source = format!(
            "; inner_wall_line_width = 0.45\n{}",
            middle_layer(&format!(
                ";TYPE:Perimeter\n{}",
                wall_of(2, "loop", 0.0, 10.0, 1.0)
            ))
        );
        for impossible in [f64::NAN, f64::INFINITY, -1.0, 1e9] {
            let config = Config {
                extra_flow: impossible,
                ..Config::default()
            };
            let out = run(&source, &config);
            assert!(!out.contains("NaN"), "{impossible}");
            let top = automatic_flow(0.2, Some(0.45), MAX_EXTRA_FLOW);
            let (low, high) = apply(&source, &config).stats.flow.expect("metered");
            assert!(
                (1.0..=top).contains(&low) && (1.0..=top).contains(&high),
                "{impossible} gave {low} to {high}"
            );
        }
    }

    /// A number on the command line is the answer, whatever the file states,
    /// so a print can be tested at a flow the geometry would never pick.
    #[test]
    fn a_flow_given_on_the_command_line_overrides_the_file() {
        let source = format!(
            "; inner_wall_line_width = 0.35\n{}",
            middle_layer(&format!(
                ";TYPE:Perimeter\n{}",
                wall_of(2, "loop", 0.0, 10.0, 1.0)
            ))
        );
        let config = Config {
            wall_flow: Some(1.1),
            ..Config::default()
        };
        let out = run(&source, &config);
        assert!(out.contains("E1.10000 ; loop1"), "{out}");
    }

    /// The multiplier is booked apart from the flow the geometry asks for, so
    /// a climbing or capped bead books only the percentage and not the step it
    /// was already metered for.
    #[test]
    fn the_multiplier_is_booked_apart_from_the_flow_it_scales() {
        let source = middle_layer(&format!(
            ";TYPE:Perimeter\n{}",
            wall_of(2, "loop", 0.0, 10.0, 1.0)
        ));
        let config = Config {
            wall_flow: Some(1.05),
            ..Config::default()
        };
        let outcome = apply(&source, &config);
        // The fixture lays 40 mm metered for its own geometry, 8 mm of it on
        // the bed, which is never scaled; 5% of the 32 mm above it is 1.6.
        assert!(
            (outcome.stats.multiplier_filament - 1.6).abs() < 1e-9,
            "{:?}",
            outcome.stats
        );
        assert!(
            (outcome.stats.filament - 41.6).abs() < 1e-9,
            "{:?}",
            outcome.stats
        );
    }

    #[test]
    fn a_multiplier_of_one_leaves_every_bead_metered_as_sliced() {
        let source = middle_layer(&format!(
            ";TYPE:Perimeter\n{}",
            wall_of(2, "loop", 0.0, 10.0, 1.0)
        ));
        let outcome = apply(&source, &plain());
        assert_eq!(outcome.stats.multiplier_filament, 0.0);
        assert!(outcome.gcode.contains("E1 ; loop2"), "{}", outcome.gcode);
        assert!(outcome.gcode.contains("E1 ; loop1"), "{}", outcome.gcode);
    }

    /// The visible wall is brought toward the loop behind it by half of what
    /// the multiplier would have added as flow, closing the same volume across
    /// the staggered joint without putting more material into the part.
    #[test]
    fn the_visible_wall_is_drawn_in_toward_the_loop_behind_it() {
        let source = with_skin_width(&middle_layer(&format!(
            ";TYPE:Perimeter\n{}{}",
            wall_of(1, "loop", 0.6, 8.8, 1.0),
            skin()
        )));
        let out = run(&source, &drawn_in());
        // A 0.4 mm wall is laid 0.357 mm from its neighbour at these layers, so
        // 1.3 of the flow widens it by 0.107 and it moves in by half of that,
        // 0.054, on every side. The bead gains 1.3 of the flow it had, over a
        // path 0.9893 of its old length.
        assert!(out.contains("G1 X9.946 Y0.054 E1.28607 ; skin1"), "{out}");
        assert!(out.contains("G1 X9.946 Y9.946 E1.28607"), "{out}");
        assert!(out.contains("G1 X0.054 Y9.946 E1.28607"), "{out}");
        assert!(out.contains("G1 X0.054 Y0.054 E1.28607"), "{out}");
        // The travel that reaches the loop has to land where it now starts.
        assert!(out.contains("G1 X0.054 Y0.054 F9000"), "{out}");
    }

    /// The offset and the flow are two halves of one answer, so where the flow
    /// is derived the offset has to move with it. A file stating a wall width
    /// the geometry reads a higher flow off draws the visible wall in further
    /// than one stating a width it reads a lower flow off.
    #[test]
    fn the_visible_wall_follows_the_flow_the_file_asked_for() {
        let walls = format!(
            ";TYPE:Perimeter\n{}{}",
            wall_of(1, "loop", 0.6, 8.8, 1.0),
            skin()
        );
        let inset = |stated: &str| {
            let source = format!(
                "; external_perimeter_extrusion_width = 0.4\n; inner_wall_line_width = {stated}\n{}",
                middle_layer(&walls)
            );
            let out = run(&source, &Config::default());
            let bead = out
                .lines()
                .find(|line| line.ends_with("; skin1"))
                .unwrap_or_else(|| panic!("the visible wall:\n{out}"))
                .to_owned();
            Line::parse(&bead).y.expect("a Y")
        };
        // Half of (flow - 1) times the 0.357 mm spacing the visible wall is
        // laid at: 0.0059 at the narrow width against 0.0030 at the wide one.
        let (narrow, wide) = (inset("0.35"), inset("0.65"));
        assert!((narrow - 0.0059).abs() < 5e-4, "narrow wall at {narrow}");
        assert!((wide - 0.0030).abs() < 5e-4, "wide wall at {wide}");
        assert!(narrow > wide, "{narrow} should sit further in than {wide}");
    }

    /// A ring does not return exactly to where it started: a slicer stops the
    /// last bead short so the two ends do not pile up at the seam. Measured
    /// over 308 loops of two real OrcaSlicer files, every one lands 0.0385 to
    /// 0.0411 mm short — the `seam_gap` default, a tenth of a 0.4 mm nozzle —
    /// and not one closes to the micron this used to demand. On a real file
    /// that left the visible wall scaled but never moved, which grows the part
    /// by half the width every bead gained.
    #[test]
    fn a_ring_stopped_short_of_its_seam_is_still_moved() {
        let source = with_skin_width(&middle_layer(&format!(
            ";TYPE:Perimeter\n{}{}",
            wall_of(1, "loop", 0.6, 8.8, 1.0),
            seamed_skin(0.04)
        )));
        let out = run(&source, &Config::default());
        let bead = out
            .lines()
            .find(|line| line.ends_with("; skin1"))
            .unwrap_or_else(|| panic!("the visible wall:\n{out}"));
        // A 0.4 mm wall laid at 0.357 mm spacing, metered at the 1.025 its
        // geometry asks for, moves 0.004 inward.
        let moved = Line::parse(bead).y.expect("a Y");
        assert!(
            (moved - 0.004).abs() < 1e-9,
            "a ring left 0.04 mm short must still be drawn in: {bead}"
        );
    }

    /// The gap is there so the seam does not get a double bead, so offsetting
    /// the ring must not quietly pull it shut. Every vertex the loop was drawn
    /// through is offset, the closing one included.
    #[test]
    fn offsetting_a_ring_leaves_its_seam_gap_where_it_was() {
        let source = with_skin_width(&middle_layer(&format!(
            ";TYPE:Perimeter\n{}{}",
            wall_of(1, "loop", 0.6, 8.8, 1.0),
            seamed_skin(0.04)
        )));
        let out = run(&source, &Config::default());
        let beads: Vec<(f64, f64)> = out
            .lines()
            .skip_while(|line| !line.ends_with("; skin1"))
            .take(4)
            .map(|line| {
                let parsed = Line::parse(line);
                (parsed.x.expect("an X"), parsed.y.expect("a Y"))
            })
            .collect();
        assert_eq!(beads.len(), 4, "the four beads of the wall:\n{out}");
        let closes = beads[3];
        // The loop now starts at (0.004, 0.004) and its last bead has to stop
        // the same 0.04 short of it as the slicer left.
        assert!(
            (closes.0 - 0.004).abs() < 1e-9 && (closes.1 - 0.044).abs() < 1e-9,
            "the seam gap must survive: {closes:?}"
        );
    }

    /// An open fragment has no inside to move toward. A thin wall breaks into
    /// them, and offsetting one drags a visible surface sideways for no
    /// reason, so the two ends being a whole bead apart is where a ring stops
    /// being a ring.
    #[test]
    fn an_open_fragment_is_left_exactly_where_it_was() {
        let source = with_skin_width(&middle_layer(&format!(
            ";TYPE:Perimeter\n{}{}",
            wall_of(1, "loop", 0.6, 8.8, 1.0),
            // The stated width is 0.4, and 1 mm apart is well past it.
            seamed_skin(1.0)
        )));
        let out = run(&source, &Config::default());
        let bead = out
            .lines()
            .find(|line| line.ends_with("; skin1"))
            .unwrap_or_else(|| panic!("the visible wall:\n{out}"));
        assert_eq!(
            Line::parse(bead).y.expect("a Y"),
            0.0,
            "an open fragment must not be moved: {bead}"
        );
    }

    /// A bead widens about its own centre, so a wall moved in by half of what
    /// it gains reaches further into the joint while the face the eye lands on
    /// does not move at all. The part measures what it was sliced to measure.
    #[test]
    fn the_visible_wall_keeps_the_dimension_it_was_sliced_to() {
        let source = with_skin_width(&middle_layer(&format!(
            ";TYPE:Perimeter\n{}{}",
            wall_of(1, "loop", 0.6, 8.8, 1.0),
            skin()
        )));
        let out = run(&source, &drawn_in());
        let bead = out
            .lines()
            .find(|line| line.ends_with("; skin1"))
            .expect("the visible wall");
        let shifted = Line::parse(bead).y.expect("a Y");

        let (nominal, flow, height) = (0.4, 1.3, 0.2);
        // The area scales, not the nominal width, so the bead keeps its round
        // caps and gains `flow - 1` of its spacing.
        let widened =
            flow * bead_spacing(height, nominal) + height * (1.0 - std::f64::consts::FRAC_PI_4);
        let outer_face = |centre: f64, width: f64| centre - width / 2.0;
        assert!(
            // The coordinate itself is written on a micron grid.
            (outer_face(shifted, widened) - outer_face(0.0, nominal)).abs() < 5e-4,
            "the wall ran at 0.0 and now runs at {shifted}, and widening it by \
             {} must leave its outer face where it was",
            widened - nominal
        );
    }

    /// Flow per mm is what the slicer metered plus what the widening asks for,
    /// so a path a shade shorter carries proportionally less of it.
    #[test]
    fn a_wall_drawn_in_carries_the_filament_its_new_width_asks_for() {
        let source = with_skin_width(&middle_layer(&format!(
            ";TYPE:Perimeter\n{}{}",
            wall_of(1, "loop", 0.6, 8.8, 1.0),
            skin()
        )));
        let out = run(&source, &drawn_in());
        // Each side runs 9.893 of the 10 mm it did, at 1.3 the flow.
        assert!(!out.contains("E1.00000 ; skin1"), "{out}");
        assert!(out.contains("E1.28607 ; skin1"), "{out}");
    }

    /// A hole is emitted clockwise, so the same rule moves its wall out of the
    /// hole and into the material around it.
    #[test]
    fn a_hole_is_opened_up_rather_than_closed() {
        let mut hole = String::from(";TYPE:External perimeter\nG1 X0.00 Y0.00 F9000\n");
        for (x, y) in [(0.0, 10.0), (10.0, 10.0), (10.0, 0.0), (0.0, 0.0)] {
            hole.push_str(&format!("G1 X{x:.2} Y{y:.2} E1.0\n"));
        }
        let source = with_skin_width(&middle_layer(&format!(
            ";TYPE:Perimeter\n{}{hole}",
            wall_of(1, "loop", 0.6, 8.8, 1.0)
        )));
        let out = run(&source, &drawn_in());
        assert!(
            out.contains("G1 X-0.054 Y10.054"),
            "a hole must open, not close:\n{out}"
        );
    }

    /// A file that states no width still has its visible wall drawn in, on the
    /// reference profile the flow already falls back to. Scaling a wall
    /// without moving it grows the part by half of what it gained, so the two
    /// halves of the change have to fall back together — Cura writes its
    /// settings in a form nothing else parses, and this is that file.
    #[test]
    fn a_file_that_states_no_wall_width_falls_back_to_the_reference_profile() {
        let source = middle_layer(&format!(
            ";TYPE:Perimeter\n{}{}",
            wall_of(1, "loop", 0.6, 8.8, 1.0),
            skin()
        ));
        let out = run(&source, &drawn_in());
        // 0.45 mm at 0.2 mm layers and a flow of 1.3 is an offset of 0.061.
        assert!(out.contains("G1 X9.939 Y0.061"), "{out}");
    }

    /// Asking for no extra flow asks for no compensation either.
    #[test]
    fn a_multiplier_of_one_leaves_the_visible_wall_where_it_was() {
        let source = with_skin_width(&middle_layer(&format!(
            ";TYPE:Perimeter\n{}{}",
            wall_of(1, "loop", 0.6, 8.8, 1.0),
            skin()
        )));
        let out = run(&source, &plain());
        assert!(out.contains("G1 X10.00 Y0.00 E1 ; skin1"), "{out}");
    }

    /// The visible wall takes its place in the alternation instead of being
    /// held out of it. Three walls leave both ends of the stack flat and raise
    /// the one between them; four raise the far end, so a wall exposed on both
    /// faces has one of them raised whenever the count is even.
    #[test]
    fn the_visible_wall_anchors_an_alternation_that_runs_the_whole_stack() {
        let stack = |walls: usize| {
            let mut body = String::from(";TYPE:Perimeter\n");
            for wall in (1..walls).rev() {
                body.push_str(&wall_of(
                    1,
                    &format!("in{wall}"),
                    0.45 * wall as f64,
                    10.0 - 0.9 * wall as f64,
                    1.0,
                ));
            }
            body.push_str(";TYPE:External perimeter\n");
            body.push_str(&wall_of(1, "skin", 0.0, 10.0, 1.0));
            let out = run(&middle_layer(&body), &plain());
            loop_states(&out)
        };

        assert_eq!(
            stack(3),
            vec![
                ("in21".to_owned(), false),
                ("in11".to_owned(), true),
                ("skin1".to_owned(), false),
            ],
            "three walls: only the one between the ends"
        );
        assert_eq!(
            stack(4),
            vec![
                ("in31".to_owned(), true),
                ("in21".to_owned(), false),
                ("in11".to_owned(), true),
                ("skin1".to_owned(), false),
            ],
            "four walls: the far end is raised too"
        );
    }

    /// A wall printed `inner-outer-inner` puts the visible wall between the
    /// two halves of the stack, so a loop's place in the buffer is no longer
    /// its place in the wall: the innermost loop is printed immediately after
    /// the visible one while sitting a whole stack away from it. Numbering by
    /// buffer position leaves it and its neighbour on the same level, which is
    /// the one thing bricking exists to prevent.
    #[test]
    fn a_wall_printed_inner_outer_inner_still_alternates_by_geometry() {
        let ring = |wall: usize, tag: &str| {
            wall_of(1, tag, 0.45 * wall as f64, 10.0 - 0.9 * wall as f64, 1.0)
        };
        // Four walls, the visible one third out of the nozzle.
        let body = format!(
            ";TYPE:Perimeter\n{}{}\
             ;TYPE:External perimeter\n{}\
             ;TYPE:Perimeter\n{}",
            ring(2, "in2"),
            ring(1, "in1"),
            ring(0, "skin"),
            ring(3, "in3"),
        );
        let states = loop_states(&run(&middle_layer(&body), &plain()));

        assert_eq!(
            states,
            vec![
                ("in21".to_owned(), false),
                ("in11".to_owned(), true),
                ("skin1".to_owned(), false),
                ("in31".to_owned(), true),
            ],
            "reading outwards that is skin flat, in1 raised, in2 flat, in3 raised"
        );
    }

    /// The loop printed after the visible wall in `inner-outer-inner` is the
    /// innermost one, which on a thick wall runs further from it than any two
    /// neighbours ever do. Grouping only against the loop printed before would
    /// split it off into a contour of its own and number it from scratch.
    #[test]
    fn a_thick_wall_stays_one_contour_however_its_loops_were_sequenced() {
        let ring = |wall: usize, tag: &str| {
            wall_of(1, tag, 0.45 * wall as f64, 12.0 - 0.9 * wall as f64, 1.0)
        };
        let mut body = String::from(";TYPE:Perimeter\n");
        for wall in [4usize, 3, 2, 1] {
            body.push_str(&ring(wall, &format!("in{wall}")));
        }
        body.push_str(";TYPE:External perimeter\n");
        body.push_str(&ring(0, "skin"));
        body.push_str(";TYPE:Perimeter\n");
        // 2.25 mm from the visible wall, well past `MAX_LOOP_GAP`.
        body.push_str(&ring(5, "in5"));
        let states = loop_states(&run(&middle_layer(&body), &plain()));

        assert_eq!(
            states,
            vec![
                ("in41".to_owned(), false),
                ("in31".to_owned(), true),
                ("in21".to_owned(), false),
                ("in11".to_owned(), true),
                ("skin1".to_owned(), false),
                ("in51".to_owned(), true),
            ],
            "the innermost loop belongs to the wall, not to a contour of its own"
        );
    }

    /// One wall shows one visible loop, so a second one is a second wall
    /// however close it runs. Grouping purely by "runs beside anything already
    /// in this contour" chains a part's islands together as each joined loop
    /// widens the contour's reach: measured on a 2-wall Benchy, 61 contours
    /// held two walls and one held nine, and every wall after the first was
    /// numbered from the first wall's visible loop.
    #[test]
    fn each_visible_wall_opens_a_contour_of_its_own() {
        // Three islands a millimetre apart, closer than two loops of one wall.
        let mut body = String::new();
        for (island, origin) in [(1, 0.0), (2, 11.0), (3, 22.0)] {
            body.push_str(";TYPE:Perimeter\n");
            body.push_str(&wall_of(1, &format!("in{island}"), origin + 0.45, 9.1, 1.0));
            body.push_str(";TYPE:External perimeter\n");
            body.push_str(&wall_of(1, &format!("out{island}"), origin, 10.0, 1.0));
        }
        let states = loop_states(&run(&middle_layer(&body), &plain()));

        assert_eq!(
            states,
            vec![
                ("in11".to_owned(), true),
                ("out11".to_owned(), false),
                ("in21".to_owned(), true),
                ("out21".to_owned(), false),
                ("in31".to_owned(), true),
                ("out31".to_owned(), false),
            ],
            "every island must be numbered from its own visible wall"
        );
    }

    /// An arc moves with the rest of the loop: it keeps the centre it was
    /// drawn about, its radius changes by the offset, and the `I`/`J` that
    /// name the centre from its start point are restated because that start
    /// point moved. Without the restatement the printer sweeps the old centre
    /// and the bead spirals away from the loop it belongs to.
    #[test]
    fn a_visible_wall_drawn_with_an_arc_moves_with_its_centre() {
        let arc = ";TYPE:External perimeter\n\
             G1 X0.00 Y0.00 F9000\n\
             G1 X10.00 Y0.00 E1.0 ; arcskin\n\
             G2 X10.00 Y10.00 I0 J5 E1.0\n\
             G1 X0.00 Y10.00 E1.0\n\
             G1 X0.00 Y0.00 E1.0\n";
        let source = with_skin_width(&middle_layer(&format!(
            ";TYPE:Perimeter\n{}{arc}",
            wall_of(1, "loop", 0.6, 8.8, 1.0)
        )));
        let out = run(&source, &drawn_in());
        // The arc turns clockwise, so the material is on the far side of its
        // centre and the offset takes the radius from 5 out to 5.054.
        assert!(
            out.contains("G2 X10.000 Y10.054 I0.000 J5.054"),
            "the arc must be redrawn about the centre it kept: {out}"
        );
        // Start (10.000, -0.054) plus J puts the centre back at (10, 5).
        assert!(
            out.contains("G1 X10.000 Y-0.054 E1.29311 ; arcskin"),
            "{out}"
        );
    }

    /// An open fragment has no inside, so there is no direction to move it in.
    #[test]
    fn an_open_run_of_visible_wall_is_not_moved() {
        let open = ";TYPE:External perimeter\n\
             G1 X0.00 Y0.00 F9000\n\
             G1 X10.00 Y0.00 E1.0 ; open\n\
             G1 X10.00 Y10.00 E1.0\n";
        let source = with_skin_width(&middle_layer(&format!(
            ";TYPE:Perimeter\n{}{open}",
            wall_of(1, "loop", 0.6, 8.8, 1.0)
        )));
        let out = run(&source, &drawn_in());
        assert!(out.contains("G1 X10.00 Y0.00 E1.30000 ; open"), "{out}");
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
        let out = run(&source, &plain());
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
        let out = run(&source, &plain());
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
        let out = run(&source, &plain());
        let raised = out
            .lines()
            .find(|line| line.ends_with("loop2"))
            .map(|line| Line::parse(line).e.expect("an extrusion"))
            .unwrap_or_else(|| panic!("loop2 missing from:\n{out}"));
        // The column below stands 0.1 above the 0.6 plane and the nozzle is at
        // 0.75, so the bead spans 0.05 of a layer metered for 0.1.
        assert_eq!(raised, 0.25, "half the flow of a 0.5 bead:\n{out}");
    }

    /// The region buffer and the loop list are reused between regions, so a
    /// second wall in the same layer has to be grouped and numbered from
    /// scratch rather than from whatever the first left.
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
            wall_flow: Some(2.0),
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
        assert_eq!(delta("; loop1"), 2.0, "the loop on the plane is doubled");
        assert_eq!(delta("; loop2"), 2.0, "and so is the raised one");
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
            ..Config::default()
        };
        let out = run(&source, &config);
        assert_eq!(
            loop_states(&out),
            vec![("loop1".to_owned(), true), ("loop2".to_owned(), false)],
            "{out}"
        );
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
            wall_flow: Some(1.3),
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
        let out = run(&source, &plain());

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
            wall_flow: Some(2.0),
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
