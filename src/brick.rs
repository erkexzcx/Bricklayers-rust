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

use crate::gcode::feature::{Feature, is_layer_marker};
use crate::gcode::{Code, Extruder, Line, Lines, MAX_LINE, Modal, repaired, write_e};
use crate::geometry::Edge;
use crate::geometry::{Arc, Cells, Trace, footprint, inset};
use crate::scan::{BRICK_STAMP, FALLBACK_Z_FEEDRATE, Markerless, Survey, is_a_height};

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

/// How far apart, in mm of path, an arc is sampled when a loop is measured or
/// probed.
///
/// A `G2`/`G3` states only where it ends, so a loop read off its endpoints
/// alone is read off its chords: two concentric arc-fitted loops a bead apart
/// whose slicer cut them at different angles then measure a chord's bulge
/// apart rather than a bead. At a 10 mm radius a 45° disagreement puts their
/// endpoints 7.5 mm from each other, far past [`MAX_LOOP_GAP`], so every loop
/// of a curved wall became a contour of its own and the whole stagger was lost
/// — which is every curved wall a slicer with arc fitting on emits, and that
/// is the Bambu Studio default. A quarter of the gap being tested leaves the
/// worst sampling error an eighth of it, and it is about the spacing a slicer
/// puts between the vertices of a loop it did *not* fit an arc to, so a curved
/// wall is probed no more finely than a straight one.
const ARC_STEP: f64 = MAX_LOOP_GAP / 4.0;

/// How much of one loop's probed path has to run within [`MAX_LOOP_GAP`] of
/// another before the two are taken for loops of the same wall.
///
/// A wall's loops are the last one offset inwards, so nearly every point of
/// one lies a bead width from the other; where one runs on past the end of the
/// one it followed, the shorter of the two is still beside the longer along
/// the whole of itself, which is why the share is measured both ways round and
/// the better answer taken.
///
/// A second wall passing close by is not like that. A hole's loop running
/// beside an island's touches it over a short stretch and leaves the rest of
/// itself elsewhere: a 5 mm hole 1.2 mm from a straight wall has a quarter of
/// its path within the gap. Asking only whether the two come close *anywhere*
/// pulled the hole into the island's contour, where its loops were ordered by
/// their distance to the island's visible wall — a distance that drifts layer
/// to layer on anything tapered or curved, so the hole's stagger flipped from
/// one layer to the next, and the hole's own visible wall was left in a
/// contour of its own that is never raised.
const BESIDE_SHARE: f64 = 0.5;

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

/// How much of a loop's path has to stand on what the layer below left proud
/// before the loop is metered as being laid on a raise.
///
/// A different question from [`CAP_SHARE`] and biased the other way. Getting
/// this wrong in the direction of "there is no raise under me" feeds a bead
/// for a whole layer of gap when the raise below has already filled half of
/// it — twice the material the gap can hold — while getting it wrong the other
/// way leaves a bead a little short, which an internal wall absorbs. So it
/// sits low.
///
/// It can sit low because the two populations barely touch. Measured over two
/// real Benchy slices, 95% to 99% of loops laid on the plane share a tenth of
/// their path or less with the raise below, while a loop carrying a raised
/// column on has 0.4 to 1.0 of its path over one — the wall shifts sideways as
/// it climbs, so even an unbroken column rarely reaches 1.0. The valley
/// between the two is empty, and [`CAP_SHARE`] would cut straight through the
/// upper population.
const SEAM_SHARE: f64 = 0.25;

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

/// The most [`flow_ceiling`] approaches on any geometry, which is what bounds
/// a flow a caller pinned on a file that states no width to solve one against.
///
/// The ceiling subtracts a positive term from 2 for every layer a printer
/// lays, so it is under 2 everywhere and reaches 2 only in the limit of an
/// infinitely thin one.
const FLOW_LIMIT: f64 = 2.0;

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
    ///
    /// A pinned flow reaches the nozzle by the same road as a derived one, so
    /// it is held to the same bounds rather than taken on trust. One that is
    /// not a number puts NaN in an `E` word; one under 1 takes material off
    /// every wall while [`Pass::skin_offset`] turns negative, so
    /// [`Pass::move_walls`] gives up and the visible wall is scaled without
    /// being moved — which is the case that grows the part. The ceiling is the
    /// bead model's own, and where the file states no geometry to solve it
    /// against, the limit it approaches stands in.
    fn flow_at(&self, height: f64, width: Option<f64>) -> f64 {
        let Some(pinned) = self.wall_flow else {
            return automatic_flow(height, width, self.extra_flow);
        };
        if !pinned.is_finite() {
            return 1.0;
        }
        let ceiling = width
            .filter(is_a_height)
            .filter(|_| is_a_height(&height))
            .map(|width| bead_spacing(height, width))
            .filter(is_a_height)
            .map(|spacing| flow_ceiling(height, spacing))
            .unwrap_or(FLOW_LIMIT);
        // Not `clamp`, which panics where the geometry puts the ceiling under 1.
        pinned.min(ceiling).max(1.0)
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
    /// Perimeter loops seen, the visible wall included and fillers excluded.
    /// Only the hidden ones can ever be raised, so this is about twice the
    /// pool [`Stats::raised`] is drawn from.
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
        // A line no reader will hold whole arrives in pieces, and asking which
        // costs a copy: the text just handed over borrows the reader it would
        // be asked of. Every piece of such a line is about the cap long, so
        // nothing shorter can open one — and the line every real file is made
        // of never asks a second question. Once one is open the question has
        // to be asked of every line, because the piece that closes it can be
        // a few bytes short of the cap.
        if !pass.spilling && raw.text.len() < MAX_LINE {
            pass.feed(raw.text, raw.bytes)?;
            continue;
        }
        let held = raw.bytes.to_owned();
        // The last piece of a long line is partial too — it was assembled out
        // of what the read before it carried over — so anything answering no
        // is a line of its own, and the one before it is finished.
        if lines.partial() {
            pass.spill(&held)?;
            continue;
        }
        pass.rejoin()?;
        pass.feed(&repaired(&held), &held)?;
    }
    pass.rejoin()?;
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
    /// Where the move names both of its coordinates, which is what a travel
    /// has to name to be handed a whole new corner: what a move leaves unnamed
    /// it inherits, and the line ahead of a loop is not one of the loop's own
    /// vertices to inherit from.
    xy: Option<(f64, f64)>,
    /// True where the line names an `X`, a `Y` or both, which is whether a new
    /// place can be written into it at all. A bead running along one axis names
    /// one word and is still a vertex of its loop: the axis it leaves out it
    /// inherits from the bead before it, which the offset has already moved
    /// onto the new ring.
    places: bool,
    /// Where the move ends, with the axes it left unnamed carried forward, so
    /// a loop's path can be walked without re-reading the region.
    at: (f64, f64),
    /// Centre and direction of a `G2`/`G3`, so its path is followed round
    /// rather than cut across. Resolved from wherever the move started, so an
    /// `R`-form arc — which names a radius and no centre at all — is followed
    /// round the same curve as an `I`/`J` one.
    arc: Option<Arc>,
    /// True where the line is a `G2`/`G3`, whatever came of reading its
    /// centre. An arc whose centre could not be resolved is not a straight
    /// move: the two ends are where the slicer put them and everything
    /// between them is not, so a loop holding one is left exactly as sliced
    /// rather than moved along a chord it was never drawn on.
    curved: bool,
    extrudes: bool,
    /// True where the line names an `X` or a `Y`, so it is the one that
    /// settles where in the plane the nozzle goes next. A height move after it
    /// leaves the nozzle exactly where it left it.
    steers: bool,
    /// True where the line decides where the nozzle is next, so nothing after
    /// it in a lead can undo a height set on it.
    positions: bool,
    /// True where a height change can ride this line instead of stopping the
    /// toolhead for one of its own. Extrusions and wipes are excluded — their
    /// path is laid against the layer below.
    ///
    /// A comment does not exclude one. Slicers that annotate their moves put
    /// one on every travel, and refusing those left every loop of such a file
    /// with a `G1 Z` of its own; the stamp goes in front of whatever the line
    /// already said instead, which is where a reader looks for it.
    carries: bool,
    /// The `E` convention this line's own words were read in.
    ///
    /// A `M82`/`M83` names no coordinate and no filament, so it is held with
    /// the region around it and only reaches the printer when that region is
    /// written. Until then the switch has not happened in the stream being
    /// written, and a bead metered back out in the convention that follows it
    /// turns an absolute position into a relative delta.
    absolute: bool,
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

/// Samples one arc may be cut into. A corrupt `I`/`J` can name a radius no
/// bed holds, and a walk metered in millimetres of it would not end; at
/// [`ARC_STEP`] this ceiling is two kilometres of path.
const MAX_ARC_SAMPLES: usize = 4096;

/// The points one extruding move lays down: an arc followed round its own
/// curve at [`ARC_STEP`], and the end point of anything else.
///
/// Written out rather than collected so a loop can be probed against every
/// other loop of its region without allocating once per arc per comparison.
struct Along {
    /// Centre, radius, opening angle and signed sweep, as [`footprint::turn`]
    /// reports them. `None` for a straight move.
    turn: Option<((f64, f64), f64, f64, f64)>,
    steps: usize,
    step: usize,
    end: (f64, f64),
}

impl Along {
    fn new(from: (f64, f64), to: (f64, f64), arc: Option<Arc>) -> Self {
        let turn = arc.and_then(|arc| footprint::turn(from, to, arc));
        let steps = match turn {
            Some((_, radius, _, sweep)) => {
                let length = radius * sweep.abs();
                ((length / ARC_STEP).ceil() as usize).clamp(1, MAX_ARC_SAMPLES)
            }
            None => 1,
        };
        Self {
            turn,
            steps,
            step: 1,
            end: to,
        }
    }

    /// A move that lays nothing down.
    fn nowhere() -> Self {
        Self {
            turn: None,
            steps: 0,
            step: 1,
            end: (0.0, 0.0),
        }
    }
}

impl Iterator for Along {
    type Item = (f64, f64);

    fn next(&mut self) -> Option<(f64, f64)> {
        let step = self.step;
        if step > self.steps {
            return None;
        }
        self.step += 1;
        // The last sample is the move's own end point, taken from the file
        // rather than from the angle, so a loop still closes where the slicer
        // closed it however the arc was sampled.
        if step == self.steps {
            return Some(self.end);
        }
        let (centre, radius, start, sweep) = self.turn?;
        let angle = start + sweep * step as f64 / self.steps as f64;
        Some((
            centre.0 + radius * angle.cos(),
            centre.1 + radius * angle.sin(),
        ))
    }
}

/// True for a region a slicer lays between the loops of a wall rather than as
/// one of them.
///
/// Gap fill goes where two loops could not both fit; a thin wall is what the
/// wall becomes where it narrows to less than two beads. Both arrive in the
/// middle of a wall, both have to be buffered with it so its loops stay in one
/// contour, and neither may be raised: a thin wall's two faces are both the
/// visible one, so half a layer of step on it is half a layer of step on the
/// outside of the part.
fn is_filler(feature: Feature) -> bool {
    matches!(feature, Feature::GapFill | Feature::ThinWall)
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
    /// One past this loop's last extruding line. What lies between it and
    /// `end` is the tail the slicer wrote after the bead — a retraction, a
    /// wipe, the travel out of the region — which lays nothing and so must
    /// never be scaled by the flow the bead was given.
    beads: usize,
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
    /// True where every bead of this loop was laid by a region that fills
    /// between the wall's loops rather than being one of them — gap fill, or a
    /// thin wall. It is buffered with the wall so that the loops either side
    /// of it stay in one contour, but it is not one of them: it takes no place
    /// in the alternation and is never raised.
    filler: bool,
    raised: bool,
    /// True where nothing stands on this loop on the next layer, so it has to
    /// finish flat whatever the parity says.
    capped: bool,
    /// Layers this loop's own column has stood for. Zero where the column
    /// begins on this layer, so its first bead climbs from the plane rather
    /// than being raised to an offset nothing under it earned.
    steps: usize,
    /// True where the material this loop is laid on was left standing proud by
    /// the layer below. Read off that layer's own footprint rather than
    /// assumed from this loop's parity, which can differ from the one the
    /// column had beneath it.
    on_a_raise: bool,
    /// True where that raise was a column still climbing rather than one that
    /// had settled, so it stood at half the offset. Measured for the same
    /// reason: how old a column is cannot be read off its own loop's age.
    on_a_climb: bool,
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
    /// Cells the layer below was left standing proud in, so a bead can be
    /// metered for the gap it really crosses rather than for the one its own
    /// parity implies.
    standing: Cells,
    /// The part of `standing` a column had only climbed half way to. A raise
    /// reaches its offset over [`RAMP`] layers, so what is under a bead is one
    /// of exactly two heights, and which one it is has to be measured for the
    /// same reason the raise itself does.
    climbed: Cells,
    /// The same for the layer being written, filled as each loop is settled
    /// and handed to `standing` when the layer closes.
    rising: Cells,
    climbing: Cells,
    /// Cells of the loop being settled, kept between calls so the walk that
    /// weighs a loop against the layers either side of it can hand its path
    /// to `rising` rather than be repeated for it.
    path: Vec<u32>,
    /// True once a refused walk has been reported. A print is already running
    /// by the time this pass sees the file, so a move no printer makes is a
    /// warning and not a failure — said once, rather than once per loop.
    warned: bool,
    layer_markers: bool,
    /// Layer boundaries for a file that carries no marker, driven off the same
    /// beads the survey drove it off. The two index the same per-layer sets, so
    /// a rule they did not share exactly would consult them for the wrong
    /// layer.
    markerless: Markerless,
    /// Layer height to use where the file measured none, which is every layer
    /// of a file sliced at a fixed height.
    height: f64,
    /// Height the file measured for each layer, empty unless the slicer varied
    /// them. A raise is half of the layer it belongs to, so an adaptive slice
    /// has as many raises as it has heights.
    heights: Vec<f64>,
    out: W,
    extruder: Extruder,
    /// The positioning mode and units every coordinate is read in. A `G91` or
    /// `G20` section is custom G-code, so it is measured and then written back
    /// exactly as it was found.
    modal: Modal,
    feature: Feature,
    layer: usize,
    started: bool,
    layer_z: f64,
    /// Lowest height this layer has commanded, which is where its beads sit.
    /// Cleared as each layer opens, and `None` until the layer names a height
    /// of its own.
    floor: Option<f64>,
    nozzle_z: Option<f64>,
    /// Rate for the Z moves this pass inserts.
    z_feedrate: f64,
    /// Feedrate the output stream is currently left in, since `F` is modal and
    /// an inserted Z move would otherwise hand its own rate to the next print.
    feedrate: Option<f64>,
    /// Text of the region being buffered. Cleared and refilled at each flush,
    /// so it only ever holds one perimeter region.
    arena: Vec<u8>,
    buffer: Vec<Buffered>,
    loops: Vec<Loop>,
    /// True while the region being written has had the lines the slicer wrote
    /// after its last bead held back for the layer that follows it. The region
    /// has not really ended, so nothing is written to settle the nozzle:
    /// what was held runs before anything else does.
    holding: bool,
    /// True while a line too long to be held whole is being copied through in
    /// pieces, so the newline it is owed has not been written yet.
    spilling: bool,
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
            standing: Cells::on(footprint::Grid::default()),
            climbed: Cells::on(footprint::Grid::default()),
            rising: Cells::on(footprint::Grid::default()),
            climbing: Cells::on(footprint::Grid::default()),
            path: Vec::new(),
            warned: false,
            layer_markers: survey.layer_markers,
            markerless: Markerless::default(),
            height: uniform.unwrap_or(survey.layer_height),
            // A height given on the command line is the one the caller wants
            // used, so it stands in for the measurement rather than beside it.
            heights: match uniform {
                Some(_) => Vec::new(),
                None => survey.layer_heights.clone(),
            },
            out,
            extruder: Extruder::new(),
            modal: Modal::new(),
            feature: Feature::Other,
            layer: 0,
            started: false,
            layer_z: 0.0,
            floor: None,
            nozzle_z: None,
            z_feedrate: survey.z_feedrate.unwrap_or(FALLBACK_Z_FEEDRATE),
            feedrate: None,
            arena: Vec::new(),
            buffer: Vec::new(),
            loops: Vec::new(),
            holding: false,
            spilling: false,
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

    fn feed(&mut self, raw: &str, bytes: &[u8]) -> io::Result<()> {
        let line = Line::parse_bytes(raw, bytes);
        if let Some(marker) = line.marker() {
            if is_layer_marker(marker) {
                self.flush()?;
                // Slicers re-declare the region after a layer change, and some
                // open the next wall with a stray segment before they do.
                // Carrying the old region across would buffer that segment as
                // a perimeter loop of its own.
                self.feature = Feature::Other;
                self.layer += usize::from(std::mem::replace(&mut self.started, true));
                self.close_layer();
                return self.push(line.origin());
            }
            if let Some(feature) = Feature::from_marker(marker) {
                // A wall's loops are grouped and numbered as one, so the two
                // regions a slicer splits it across stay in one buffer: the
                // visible wall takes its place in the same alternation as the
                // loops behind it.
                //
                // Gap fill is laid where two loops of that same wall could not
                // both fit, so its region arrives in the middle of the wall
                // too. Ending the wall there numbers the halves either side of
                // it separately, and the half without the visible wall in it
                // falls back to being numbered from a fixed end — which on a
                // four-loop wall split down the middle inverts it. A thin wall
                // arrives the same way and for the same reason, where the wall
                // narrows to less than two beads for a stretch.
                let with_the_wall = |feature: Feature| feature.is_perimeter() || is_filler(feature);
                let continues = self.feature.is_perimeter() && feature.is_perimeter()
                    || !self.loops.is_empty()
                        && with_the_wall(self.feature)
                        && with_the_wall(feature);
                if !self.loops.is_empty() && !continues {
                    self.flush()?;
                }
                self.feature = feature;
                if continues && !self.buffer.is_empty() {
                    self.buffer(line, self.at);
                    return Ok(());
                }
                return self.keep(line, self.at);
            }
        }

        // A `G91` or `G20` section is custom G-code — a colour change, an MMU
        // swap, a timelapse or a layer-change script — and nothing this pass
        // writes says what it means inside one. The region is closed while a
        // plain move still does, so a standing raise is put back before the
        // section begins.
        if matches!(line.code, Code::RelativePosition | Code::Inches) && self.modal.is_plain() {
            self.flush()?;
        }
        let moved = self.modal.apply(&line);

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
                // The toolhead has not moved and the frame it is named in has,
                // so the next move starts from where the reset says it stands.
                let (x, y, _) = self.modal.position();
                self.at = (x, y);
            }
            _ => {}
        }

        // Klipper and Orca without a Z-hop put a layer's Z on the travel that
        // reaches the first loop, which lands inside the buffered region.
        if line.z.is_some() && (line.is_move() || line.code == Code::SetPosition) {
            // Where the move ends, not the number it names: under `G91` a
            // `G1 Z0.6` is a lift and under `G20` it is 15.24 mm.
            let z = self.modal.position().2;
            self.layer_z = z;
            // A Z-hop only ever lifts, and so does a raise this pass inserts,
            // so the lowest height the layer commands is the plane its beads
            // sit on. Taking the last one instead hands a lift emitted after
            // the region's final bead — but before the marker that flushes it
            // — to every loop of that region, which then prints in the air.
            // A file with no marker to close a layer on has no run of moves to
            // take the lowest of: its plane comes from the bead that opened
            // the layer.
            if self.layer_markers {
                self.floor = Some(self.floor.map_or(z, |floor: f64| floor.min(z)));
            }
        }

        // Every line of such a section goes straight out: no loop is buffered
        // from it, nothing rides a height, and no height move is inserted into
        // it. Only its `E` word is still settled, since the extrusion stream is
        // a running total that owes nothing to how a coordinate is read. Where
        // the section leaves the nozzle is still tracked, or the first plain
        // move after it would be traced from where the toolhead stood before
        // the section began.
        if !self.modal.is_plain() {
            self.flush()?;
            if let Some((x, y, _)) = moved {
                self.at = (x, y);
            }
            if line.z.is_some() && line.is_move() {
                self.nozzle_z = Some(self.modal.position().2);
            }
            if let Some(rate) = line.f {
                self.feedrate = Some(rate);
            }
            return self.emit(line, 1.0);
        }

        // Where the file states no layers, the first bead laid off the plane
        // the last one sat on is what opens the next one. The Z move that
        // reached it says nothing: a hop lifts and comes back down before
        // anything is extruded again, so counting Z moves counted every hop as
        // a layer and walked this pass's layer number away from the survey's.
        if !self.layer_markers
            && self.lays_a_bead(&line)
            && self.markerless.opens_a_layer(self.layer_z)
        {
            // Loops still buffered belong to the layer that is ending, and
            // they have to be written at its plane, metered for its height and
            // marked against the footprint it left. What the slicer wrote
            // after that layer's last bead stays: it is the travel this
            // layer's first raise rides.
            self.flush_before_a_layer()?;
            self.layer += usize::from(std::mem::replace(&mut self.started, true));
            self.close_layer();
            self.markerless.open(self.layer_z);
        }

        // A slicer names only the axes that change, so a move starts wherever
        // the last one left off.
        let from = self.at;
        if let Some((x, y, _)) = moved {
            self.at = (x, y);
        }

        // Gap fill only joins the buffer where there is a wall in it to keep
        // whole. Writing it straight out while loops are still buffered would
        // put it ahead of the loops the slicer laid before it.
        if self.feature.is_perimeter() || self.fills_a_buffered_wall() {
            if self.buffer.is_empty() {
                self.entry = from;
            }
            self.buffer(line, from);
            return Ok(());
        }

        self.keep(line, from)
    }

    /// True where this is a filler region that opened inside a wall this
    /// pass is still holding, so its beads belong in the buffer with it.
    fn fills_a_buffered_wall(&self) -> bool {
        is_filler(self.feature) && !self.loops.is_empty()
    }

    /// True where this line lays material down, tested exactly as
    /// [`Survey`] tests it, since the two lay a file with no markers out off
    /// the same beads.
    ///
    /// The extruder is read on a copy: the line has been neither buffered nor
    /// written, so consuming its delta here would book it twice.
    fn lays_a_bead(&self, line: &Line<'_>) -> bool {
        line.draws_in_plane()
            && line.e.is_some_and(|e| {
                let mut ahead = self.extruder;
                ahead.observe(e) > 0.0
            })
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
    /// onto. Six things it declines, passing the loop through as sliced: an
    /// open fragment, which has no inside; fewer than three points; a loop
    /// whose arcs could not be moved without distorting the circle they were
    /// drawn on; a loop holding an arc whose centre could not be read at all;
    /// a loop nothing in its lead can be given the new start point in; and the
    /// whole of the layer laid on the build plate, where there is no staggered
    /// joint to close.
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
            // The travel that reached the loop has to land where it now
            // starts, or the first bead is drawn from the corner the slicer
            // chose while its far end goes to the moved one — and where that
            // bead is an arc, its `I`/`J` are restated from a start the nozzle
            // never reached, so the whole sweep turns about a displaced
            // centre. The line to carry it is the last of the lead to steer,
            // since a height move after that one leaves the nozzle where it
            // was left, and it has to name both coordinates to be given a
            // corner at all. A loop with no such line is left exactly where
            // the slicer put it: a wall drawn as sliced still prints, and one
            // approached from the wrong place does not.
            let Some(travel) = (current.lead..current.body)
                .rev()
                .find(|&at| self.buffer[at].steers)
                .filter(|&at| self.buffer[at].xy.is_some())
            else {
                continue;
            };
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
            // An arc whose centre could not be read is not a straight move.
            // Its two ends are where the slicer put them and everything
            // between them is not, so offsetting the chord would take the bead
            // off the curve it was drawn on and leave its `R` naming a radius
            // the new ends do not span.
            if beads.iter().any(|&at| {
                let buffered = self.buffer[at];
                buffered.curved && buffered.arc.is_none()
            }) {
                continue;
            }

            // Every vertex the loop was drawn through except the one it closes
            // on, which is not a corner: it sits partway along the same edge
            // the loop starts from, a seam gap short of the start. Offered to
            // the offset as a vertex it becomes the far end of an edge that is
            // 0.04 mm long, which the miter turns end for end as soon as the
            // offset is a fraction of that — so the whole loop was declined,
            // and every wall of every real file with it, above `--extra-flow`
            // 40 or so.
            let mut ring = vec![entry];
            ring.extend(
                beads[..beads.len() - 1]
                    .iter()
                    .map(|&at| self.buffer[at].at),
            );
            // How the loop travels out of each of those vertices. An arc
            // states its centre relative to where it starts, which is the
            // vertex before it.
            let edges: Vec<Edge> = beads
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

            let Some(mut offset) = inset::offset(&ring, &edges, inward) else {
                continue;
            };
            // The loop closes where its own start moved to, less the gap the
            // slicer left there. Carrying the start's move over keeps that gap
            // exactly, where offsetting the closing point along its own normal
            // would lose the whole of it and at a wide enough offset run the
            // bead past its own seam. The vertex is a translation of one the
            // offset has already judged, by the same vector as in the file, so
            // the sweep it gives the closing bead stands in the same relation
            // to the judged one as the slicer's own did.
            offset.push((
                offset[0].0 + (closes.0 - entry.0),
                offset[0].1 + (closes.1 - entry.1),
            ));
            ring.push(closes);
            // Which can still pull the last bead off the circle it is drawn
            // on, where that bead is an arc.
            if !inset::keeps_its_arcs(&offset, &edges) {
                continue;
            }

            for (step, &at) in beads.iter().enumerate() {
                let next = step + 1;
                let was = inset::length(ring[step], ring[next], edges[step]);
                let now = inset::length(offset[step], offset[next], edges[step]);
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
            moved[travel] = Some(Moved {
                to: offset[0],
                centre: None,
                ratio: 1.0,
            });
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
    fn keep(&mut self, line: Line<'_>, from: (f64, f64)) -> io::Result<()> {
        let lays = (line.x.is_some() || line.y.is_some()) && line.e.is_some();
        let holds = self.loops.is_empty() && self.buffer.len() < TAIL && !lays;
        if holds {
            if self.buffer.is_empty() {
                self.entry = from;
            }
            self.buffer(line, from);
            return Ok(());
        }

        // Anything else ends the tail, and it has to be written out before the
        // line that ended it.
        self.flush()?;
        if line.z.is_some() && line.is_move() {
            self.nozzle_z = Some(self.modal.position().2);
        }
        if let Some(rate) = line.f {
            self.feedrate = Some(rate);
        }
        self.emit(line, 1.0)
    }

    fn buffer(&mut self, line: Line<'_>, from: (f64, f64)) {
        // A `G92` carries an `E` that sets the origin rather than asking for
        // filament, and `set_position` has already dealt with it.
        let delta = line
            .e
            .filter(|_| line.draws())
            .map(|e| self.extruder.observe(e));
        let xy = line.xy();
        // A bead that runs along one axis names one word, and an arc fitted to
        // a whole circle names neither. Asking for both read such a bead as a
        // travel, which cut the wall it belonged to in two: the halves were
        // contoured, numbered and raised apart — a half-layer step in the
        // middle of one loop — and neither of them closed, so the visible wall
        // was scaled without ever being moved.
        //
        // Arc fitting turns a run of short segments into one G2/G3. Leaving
        // those out of a loop would strand its opening arcs in the travel that
        // reaches it, to be printed before the nozzle rises.
        let extrudes = line.draws_in_plane() && delta.is_some_and(|d| d > 0.0);
        let steers = line.is_xy_move();
        let positions = steers || (line.is_move() && line.z.is_some());
        let carries = positions && line.e.is_none();
        let places = line.x.is_some() || line.y.is_some();

        let index = self.buffer.len();
        let start = self.arena.len();
        self.arena.extend_from_slice(line.origin());
        self.buffer.push(Buffered {
            start,
            end: self.arena.len(),
            e_span: line.e_span(),
            e: line.e,
            delta,
            // Where the move leaves the nozzle, not the number on the line:
            // every height this pass compares against is an absolute one.
            z: line
                .z
                .filter(|_| line.is_move())
                .map(|_| self.modal.position().2),
            f: line.f,
            xy,
            places,
            at: self.at,
            arc: line.arc_between(from, self.at),
            curved: line.code == Code::Arc,
            extrudes,
            steers,
            positions,
            carries,
            absolute: self.extruder.is_absolute(),
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
                    current.filler &= is_filler(feature);
                }
            }
        } else if steers {
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
            beads: 0,
            contour: 0,
            external: self.feature == Feature::ExternalPerimeter,
            hidden: self.feature == Feature::InternalPerimeter,
            filler: is_filler(self.feature),
            raised: false,
            capped: false,
            steps: 0,
            on_a_raise: false,
            on_a_climb: false,
            outline: None,
            points: 0,
        });
        self.travelled = false;
    }

    /// Hands the footprint this layer left standing proud to the next one,
    /// reusing both buffers rather than allocating a set per layer.
    fn close_layer(&mut self) {
        self.rising.settle();
        self.climbing.settle();
        std::mem::swap(&mut self.standing, &mut self.rising);
        std::mem::swap(&mut self.climbed, &mut self.climbing);
        self.rising.clear();
        self.climbing.clear();
        self.floor = None;
    }

    /// The height this layer's beads sit at, which every raise is measured
    /// from. The last height commanded stands in until the layer names one,
    /// since some dialects put a layer's `G1 Z` ahead of its own marker.
    fn plane(&self) -> f64 {
        self.floor
            .or_else(|| self.markerless.plane())
            .unwrap_or(self.layer_z)
    }

    /// Writes the buffered region out, but holds back the lines the slicer
    /// wrote after its last bead.
    ///
    /// A file that states no layers only confirms a boundary at the first bead
    /// laid off the previous plane, by which time the `G1 Z` that reached the
    /// new plane and the travel that reaches that bead are already buffered
    /// with the layer that is ending. Writing all of it leaves the next
    /// layer's first loop nothing to carry its height, so a raised one falls
    /// back to a `G1 Z` of its own — on the loop's start point, primed, which
    /// is the seam. Held back, it is the same lead a marked file gives that
    /// loop through [`Pass::keep`].
    ///
    /// The lines are moved rather than re-read: they have already been
    /// measured against the input stream, and reading them twice would book
    /// their filament twice.
    fn flush_before_a_layer(&mut self) -> io::Result<()> {
        let Some(last) = self.buffer.iter().rposition(|buffered| buffered.extrudes) else {
            return Ok(());
        };
        let split = last + 1;
        // The same cap [`TAIL`] puts on a held lead, for the same reason.
        if self.buffer.len() - split > TAIL {
            return self.flush();
        }
        let held: Vec<(Buffered, Vec<u8>)> = self.buffer[split..]
            .iter()
            .map(|buffered| {
                (
                    *buffered,
                    self.arena[buffered.start..buffered.end].to_owned(),
                )
            })
            .collect();
        let entry = self.buffer[last].at;
        self.buffer.truncate(split);
        self.holding = !held.is_empty();
        self.flush()?;
        self.holding = false;
        self.entry = entry;
        for (mut buffered, bytes) in held {
            buffered.start = self.arena.len();
            self.arena.extend_from_slice(&bytes);
            buffered.end = self.arena.len();
            self.buffer.push(buffered);
        }
        Ok(())
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

        let mut standing = None;
        for index in 0..self.loops.len() {
            let current = self.loops[index];
            let end = current.end;

            // A loop with nothing standing on it stays on the plane however
            // the parity fell: raising it would leave a bead half a layer
            // proud of whatever the slicer prints over it next, into a gap it
            // metered for a whole layer. `extrusion_factor` meters the half
            // gap the column below already filled.
            let offset = self.rise_of(current);
            let raise = offset > 0.0;
            let plane = self.plane();
            let target = plane + offset;
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
            let factor = self.extrusion_factor(current);
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
                let geometry = self.geometry(current);
                self.meter(current.body, current.beads, factor, geometry, raise);
            }
            // The nozzle has to come back down before whatever the slicer
            // prints next, and the travel that leaves the region can carry it
            // just as well as the lead carried the way up — unless the height
            // that follows is already going somewhere, in which case a reset
            // would fight it. That is either a `G1 Z` the slicer put in this
            // region's own tail, or, where the file states no layers, the one
            // held back for the layer about to open: both take the nozzle to
            // the next plane themselves.
            let commanded = self.buffer[current.body..end]
                .iter()
                .any(|buffered| buffered.z.is_some());
            let closing = (index + 1 == self.loops.len() && raise && !commanded && !self.holding)
                .then(|| self.carrier(current.body, end, plane))
                .flatten();
            for at in current.body..end {
                // Past the last bead the flow no longer applies: what is left
                // is the retraction and wipe that leave the loop, and scaling
                // those pulls back a length the priming move will not put back.
                let factor = if at < current.beads { factor } else { 1.0 };
                match closing {
                    Some(at_) if at_ == at => self.ride(at, plane, false, &moved)?,
                    _ => self.replay(at, factor, &moved)?,
                }
            }

            standing = raise.then_some(target);
            // Gap fill rides in the buffer to keep the wall around it whole,
            // but it is not one of the wall's loops and counting it would put
            // beads in the report that nothing was ever going to raise.
            self.loops_seen += usize::from(!current.filler);
            self.raised += usize::from(raise);
            self.capped += usize::from(current.raised && current.capped);
        }

        // Only where the nozzle is still standing at the raise. A region whose
        // last lines are the slicer's own retract and lift has already left it,
        // and writing a height there would pull the nozzle back down through
        // whatever the lift was for. A region whose tail is being held has not
        // ended at all: those lines still run before anything else, and they
        // are what takes the nozzle to the next plane.
        if !self.holding && standing.is_some_and(|target| self.nozzle_z == Some(target)) {
            self.move_z(self.plane(), false)?;
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
            let body = self.loops[index].body;
            // The travel out of one loop is pulled into the next one's lead,
            // so only the region's last loop ever holds a tail of its own —
            // and that one holds everything to the end of the region.
            let beads = (body..end)
                .rev()
                .find(|&at| self.buffer[at].extrudes)
                .map_or(body, |at| at + 1);
            let (outline, points) = self.measure(body, end);
            let current = &mut self.loops[index];
            current.end = end;
            current.beads = beads;
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
            // Gap fill runs between two loops of the wall it belongs to, so it
            // joins whatever contour is open and neither opens one nor stands
            // between the loops either side of it. Letting it break the chain
            // is the same defect as letting its marker flush the region.
            if self.loops[index].filler {
                self.loops[index].contour = contour;
                continue;
            }
            let wall = |at: usize| !self.loops[at].filler;
            let taken = self.loops[index].external
                && (opened..index).any(|at| wall(at) && self.loops[at].external);
            let joins = index > 0
                && !taken
                && (opened..index)
                    .rev()
                    .any(|at| wall(at) && self.adjacent(at, index));
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
            // Gap fill sits in the contour but is not one of its loops, so the
            // alternation counts past it: the wall keeps the numbering it
            // would have had if the slicer had found room for both loops. It
            // is never the anchor either — a loop that lays a bead of the
            // visible wall stops being a filler.
            let walls = (start..end).filter(|&at| !self.loops[at].filler).count();
            let anchor_at = (start..end).find(|&at| self.loops[at].external);
            let anchor =
                anchor_at.map(|at| (start..at).filter(|&at| !self.loops[at].filler).count());
            // Where the visible wall was printed at one end of the region, the
            // slicer worked through the stack in order and a loop's place in
            // it is its distance along the buffer. `inner-outer-inner` breaks
            // that: it prints the innermost wall last, right after the visible
            // one, which lands it a step from the anchor when it is a whole
            // stack away. Measuring the geometry is the only way to tell, and
            // it is only paid for where the order is not already monotonic.
            let ranked = anchor
                .filter(|&place| place != 0 && place + 1 != walls)
                .and(anchor_at)
                .map(|at| {
                    let mut gaps: Vec<(usize, f64)> = (start..end)
                        .filter(|&loop_| !self.loops[loop_].filler)
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
            let mut place: usize = 0;
            for at in start..end {
                if self.loops[at].filler {
                    continue;
                }
                let phase = match anchor {
                    Some(rank) => place.abs_diff(rank),
                    None if self.config.external_perimeters_first => place + 1,
                    None => walls - place,
                };
                self.loops[at].raised = !phase.is_multiple_of(2);
                place += 1;
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

    /// True when the two loops run beside each other, within
    /// [`MAX_LOOP_GAP`], over [`BESIDE_SHARE`] of one of their paths.
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

        // Beside each other along most of one of them, not merely touching
        // somewhere. Measured both ways round because a wall that widens
        // leaves the shorter loop beside the longer along the whole of
        // itself while the longer runs on past its end.
        self.beside(current, previous) || self.beside(previous, current)
    }

    /// True where more than [`BESIDE_SHARE`] of `current`'s probed path runs
    /// within [`MAX_LOOP_GAP`] of `other`'s.
    fn beside(&self, current: Loop, other: Loop) -> bool {
        if current.points == 0 {
            return false;
        }
        let stride = current.points.div_ceil(PROBES).max(1);
        let probes = current.points.div_ceil(stride);
        let limit = MAX_LOOP_GAP * MAX_LOOP_GAP;
        let enough = probes as f64 * BESIDE_SHARE;
        let (mut near, mut apart) = (0usize, 0usize);
        for (x, y) in self.points(current.body, current.end).step_by(stride) {
            let close = self.points(other.body, other.end).any(|(px, py)| {
                let (dx, dy) = (px - x, py - y);
                dx * dx + dy * dy <= limit
            });
            near += usize::from(close);
            apart += usize::from(!close);
            // Settled either way: neither the probes left nor the ones
            // already counted can change the answer.
            if near as f64 > enough {
                return true;
            }
            if (probes - apart) as f64 <= enough {
                return false;
            }
        }
        near as f64 > enough
    }

    /// The points a loop lays down, in print order.
    ///
    /// An arc is followed round its own curve rather than cut across. A
    /// `G2`/`G3` states only where it ends, so a loop read off its endpoints
    /// is read off its chords, and where a slicer cut two concentric loops at
    /// different angles their chords part company by far more than the bead
    /// between them — see [`ARC_STEP`].
    fn points(&self, from: usize, to: usize) -> impl Iterator<Item = (f64, f64)> + '_ {
        let mut at = match from {
            0 => self.entry,
            index => self.buffer[index - 1].at,
        };
        self.buffer[from..to].iter().flat_map(move |buffered| {
            let previous = std::mem::replace(&mut at, buffered.at);
            match buffered.extrudes {
                true => Along::new(previous, buffered.at, buffered.arc),
                false => Along::nowhere(),
            }
        })
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
    /// wipe follows the layer below and cannot be tilted.
    fn carrier(&self, from: usize, to: usize, z: f64) -> Option<usize> {
        // Where this range leaves the nozzle, if it commands a height at all.
        // Only this range: the nozzle also stands off the plane wherever the
        // loop before it was raised, and that is this pass's own doing rather
        // than a lift of the slicer's to be preserved.
        let commanded = self.buffer[from..to]
            .iter()
            .rev()
            .find_map(|buffered| buffered.z);
        // Nothing to carry where the range already leaves the nozzle there.
        if commanded.or(self.nozzle_z) == Some(z) {
            return None;
        }
        let index = to
            - self.buffer[from..to]
                .iter()
                .rev()
                .position(|b| b.positions)?
            - 1;
        let carrier = self.buffer[index];
        // Never ride a move that runs under a lift the slicer made on purpose:
        // pulling a Z-hop down to printing height drags the nozzle through
        // what it was lifted to clear. The lift is usually a line of its own,
        // and the travel after it names no `Z` and inherits the height — so
        // testing the candidate's own word finds nothing, accepts the travel
        // and undoes the whole hop.
        (carrier.carries && commanded.is_none_or(|had| had <= z)).then_some(index)
    }

    /// Replays a buffered move with its height set to `z`, in place of the
    /// `G1 Z` that would otherwise have been inserted after it. A move that is
    /// also being taken sideways carries both at once: dropping the height
    /// onto a line of its own instead would put a toolhead stop back on the
    /// seam of every loop the visible wall is drawn in on.
    ///
    /// The stamp goes in front of whatever the line already said, because that
    /// is where the survey and every other reader look for it: a comment is
    /// everything past the first `;`, so a stamp appended behind the slicer's
    /// own note is no longer the start of one. Nothing of the note is lost —
    /// it follows the stamp on the same line.
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
        let raw = &arena[buffered.start..buffered.end];
        let (body, said) = match raw.iter().position(|byte| *byte == b';') {
            Some(at) => {
                let kept = raw[..at]
                    .iter()
                    .rposition(|byte| !byte.is_ascii_whitespace())
                    .map_or(0, |end| end + 1);
                (&raw[..kept], Some(&raw[at + 1..]))
            }
            None => (raw, None),
        };
        // No `E` word to rescale: `Buffered::carries` is only true without one.
        let text = repaired(body);
        let line = Line::parse_bytes(&text, body);
        let written = match to {
            Some(moved) => line.write_moved(out, moved.to, moved.centre, None, Some(z))?,
            None => false,
        };
        if !written {
            line.write_z(out, z)?;
        }
        write!(out, " ; {BRICK_STAMP}{note}")?;
        if let Some(said) = said {
            out.write_all(b" ;")?;
            out.write_all(said)?;
        }
        out.write_all(b"\n")
    }

    /// Replays a buffered line, its flow scaled by `factor` and, where the
    /// visible wall is being taken sideways, its `X` and `Y` moved to `to`.
    /// The ratio beside the target is what the loop's length changed by, so
    /// flow per mm is what the slicer metered times the factor.
    fn replay(&mut self, index: usize, factor: f64, moved: &[Option<Moved>]) -> io::Result<()> {
        let buffered = self.buffer[index];
        // The length a bead is metered against is the one it is written at, so
        // the move and the ratio it changed the path by are decided together:
        // taking the ratio off a write that then does not happen meters a bead
        // for a path nothing drew. A line naming neither coordinate is the one
        // case `Line::write_moved` refuses.
        let to = moved[index].filter(|_| buffered.places);
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
        // The convention this line's own words were read in, which a `M82` or
        // `M83` still buffered behind it has not changed yet. The extruder's
        // mode belongs to the input while a region is held, so it is put back
        // the moment the value is settled.
        let reading = self.extruder.is_absolute();
        self.extruder.set_mode(e_mode(buffered.absolute));

        let Self {
            arena,
            out,
            extruder,
            ..
        } = self;
        let raw = &arena[buffered.start..buffered.end];
        let value = buffered.delta.map(|delta| extruder.advance(delta * factor));
        extruder.set_mode(e_mode(reading));

        if let Some(moved) = to {
            let text = repaired(raw);
            let line = Line::parse_bytes(&text, raw);
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

    fn emit(&mut self, line: Line<'_>, factor: f64) -> io::Result<()> {
        if line.code == Code::SetPosition {
            if let Some(value) = line.e {
                self.extruder.advance_origin(value);
            }
        }
        let Some(e) = line.e.filter(|_| line.draws()) else {
            return self.push(line.origin());
        };
        let delta = self.extruder.observe(e);
        if delta > 0.0 {
            self.filament += delta * factor;
        }
        let value = self.extruder.advance(delta * factor);
        if value == e {
            return self.push(line.origin());
        }
        line.write_e(&mut self.out, value)?;
        self.out.write_all(b"\n")
    }

    /// Copies one piece of a line too long to be held whole straight out,
    /// exactly as it arrived and with no terminator behind it.
    ///
    /// [`Lines`] hands such a line over in pieces, and those pieces are one
    /// line of the file — a comment carrying an embedded object name, or a
    /// thumbnail. A newline written between two of them would make the tail of
    /// that line a command in its own right, and the tail of a thumbnail is
    /// whatever base64 happened to spell. So nothing is written between them
    /// and the newline is owed until the whole line has gone by; see
    /// [`Pass::rejoin`].
    ///
    /// Whatever this pass is holding goes out first. The piece is copied *past*
    /// the transform rather than through it, so it must not overtake beads the
    /// slicer wrote before it.
    fn spill(&mut self, piece: &[u8]) -> io::Result<()> {
        self.flush()?;
        self.out.write_all(piece)?;
        self.spilling = true;
        Ok(())
    }

    /// Ends a line that was handed over in pieces, now that the last of them
    /// has been written. Nothing at all where no line was.
    fn rejoin(&mut self) -> io::Result<()> {
        match std::mem::take(&mut self.spilling) {
            true => self.out.write_all(b"\n"),
            false => Ok(()),
        }
    }

    /// Flow a loop's bead needs, as a multiple of what the slicer metered it
    /// for.
    ///
    /// A filler is buffered with the wall it belongs to but is not one of its
    /// loops, so it is never raised and never given the wall's flow — the
    /// multiplier is the whole of what "the wall's flow" means here, and no
    /// filler ever takes it. What is left is the gap the bead crosses, and
    /// that is measured for a filler exactly as it is for every other bead:
    /// the plane it starts from is whatever the layer below actually left
    /// under it, so a bead laid over a column standing half a layer proud has
    /// half the gap to fill and is metered for half.
    ///
    /// Both kinds of filler take it. A thin wall is plainly a bead of its own
    /// with a plane under it. Gap fill is laid into the valley between two
    /// beads and straddles whatever they did, so the plane under it is a
    /// coarser answer — but it is still a measured one, and the alternative is
    /// not "as sliced" but "metered for a whole layer over half a gap", which
    /// pours twice what the gap holds. That is also what lets gap fill count
    /// as covering the raise beneath it; see
    /// [`covers_a_raise`](crate::scan). The two go together: a region that
    /// stops a column being capped has to be metered against the column it
    /// stopped.
    fn extrusion_factor(&self, current: Loop) -> f64 {
        if current.filler {
            return self.geometry(current);
        }
        self.geometry(current) * self.multiplier()
    }

    /// How far the bead reaches, as a multiple of its own layer's height,
    /// before the multiplier.
    ///
    /// It starts on top of whatever the layer below left under it and ends at
    /// the nozzle, so the span is this layer's height plus the ground its own
    /// rise gains over that. Where the two match it spans exactly one layer
    /// and the arithmetic is skipped rather than trusted: `(h + x) - x` is not
    /// `h` in binary.
    ///
    /// Both ends are read off what actually happened rather than off the
    /// parity, because the two part company: a wall that gains or loses a loop
    /// renumbers, and a stretch the slicer calls an overhang is held flat
    /// whatever its column did. A bead metered for a parity it no longer has
    /// crosses **half** the gap it was fed for, which is the one direction
    /// that blobs.
    fn geometry(&self, current: Loop) -> f64 {
        let offset = self.rise_of(current);
        let below = self.rise_below(current);
        if offset == below {
            return 1.0;
        }
        let height = self.height();
        // Floored at nothing rather than allowed to go negative. The ground
        // can stand above the nozzle where a layer is under half the one
        // beneath it, and a negative factor is not a thin bead but a
        // retraction written mid-wall, which unprimes the nozzle and leaves
        // the extruder measuring from a position it never reached.
        ((height + offset - below) / height).max(0.0)
    }

    /// How far above its layer's plane this loop is printed.
    fn rise_of(&self, current: Loop) -> f64 {
        if current.raised {
            self.offset(current.steps, current.capped)
        } else {
            0.0
        }
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

    /// The offset the material under this loop was left standing at, measured
    /// from the layer below's height rather than this one's.
    ///
    /// Zero wherever that layer left nothing proud here, however this loop's
    /// own parity fell. Where it did leave something, how tall it stood is
    /// read off the footprint too: a column reaches its offset over [`RAMP`]
    /// layers, so what is under a bead is either the full rise or the one
    /// intermediate step of the climb.
    ///
    /// It used to be worked out from this loop's own `steps`, which is three
    /// valued — nought, one, or the object's whole age. A column opening at
    /// layer M is supported from M+2 on, so from there its loops read as old
    /// as the object and the layer below was taken for a settled raise when it
    /// was really the middle of the climb: the span came out 1.0 where the
    /// truth is 1.25, feeding the bead four fifths of the gap it crosses. That
    /// is the roof of every bridged hole and the underside of every shelf, two
    /// layers above the column's first bead.
    fn rise_below(&self, current: Loop) -> f64 {
        let Some(below) = self.layer.checked_sub(1).filter(|_| current.on_a_raise) else {
            return 0.0;
        };
        self.rise_at(if current.on_a_climb { 1 } else { RAMP }, below)
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
    /// is what costs: five sets are tested for the price of one. The last two
    /// are what the layer below left standing proud and how much of that was a
    /// column still climbing, which is what tells a bead laid on a raise from
    /// one laid on the plane and a full raise from half of one — none of the
    /// three can be read off this loop's own parity or age, and a bead metered
    /// for the wrong one crosses less of the gap it was fed for.
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
        // Both are held by this pass rather than the survey, so they are taken
        // out for the walk and handed straight back.
        let standing = self.standing.take();
        let climbed = self.climbed.take();
        let mut rising = self.rising.take();
        let mut climbing = self.climbing.take();
        let mut path = std::mem::take(&mut self.path);
        let proud = (!standing.is_empty()).then_some(&standing);
        let part_way = (!climbed.is_empty()).then_some(&climbed);

        for index in 0..self.loops.len() {
            let (share, points, traced) = self.shares(
                [above, here, below, proud, part_way],
                self.loops[index],
                &mut path,
            );
            // A walk that could not be completed says nothing about what is
            // over this loop or under it, and every one of the answers below
            // is a share of a path whose length is now unknown. The loop is
            // left on its layer's plane instead: a wall printed as the slicer
            // sliced it is a wall that prints, where a raise measured against
            // a part of a loop is a bead metered for a gap it does not cross.
            // Nothing of it is absorbed into `rising` either, so the layer
            // above measures nothing standing here — which is exactly what
            // will have been printed.
            if traced == Trace::Refused {
                self.warn_about_the_trace();
                self.loops[index].raised = false;
                self.loops[index].capped = false;
                self.loops[index].steps = object;
                self.loops[index].on_a_raise = false;
                self.loops[index].on_a_climb = false;
                continue;
            }
            let over = |set: usize| points > 0 && share[set] as f64 > points as f64 * CAP_SHARE;
            self.loops[index].capped = tops || over(0);
            self.loops[index].steps = match (over(1), over(2)) {
                (true, _) => 0,
                (_, true) => 1,
                _ => object,
            };
            self.loops[index].on_a_raise =
                points > 0 && share[3] as f64 > points as f64 * SEAM_SHARE;
            // What is climbing is a subset of what is standing, so the raise
            // below is a climbing one where most of what stands under this
            // loop is. A mix takes the settled height, which is the taller of
            // the two: reading a bead as crossing less than it does leaves it
            // a little short, and reading it as crossing more pours what the
            // gap cannot hold.
            self.loops[index].on_a_climb = share[4] * 2 > share[3];
            let current = self.loops[index];
            if self.rise_of(current) > 0.0 {
                rising.absorb(&path);
                // The intermediate step of the ramp, which the layer above
                // has to meter against rather than against the full offset.
                if current.steps < RAMP {
                    climbing.absorb(&path);
                }
            }
        }
        self.standing = standing;
        self.climbed = climbed;
        self.rising = rising;
        self.climbing = climbing;
        self.path = path;
    }

    /// Says once that a loop's path could not be walked. The user's print is
    /// already running, so this is never a failure: the loops it touches are
    /// left exactly as the slicer wrote them and the rest of the file is
    /// bricked as usual.
    fn warn_about_the_trace(&mut self) {
        if std::mem::replace(&mut self.warned, true) {
            return;
        }
        eprintln!(
            "corbel: warning: a wall move could not be followed, so the loops holding it are left on their layer's plane"
        );
    }

    /// Walks the moves that lay a loop's beads, as `(from, to, arc)`.
    ///
    /// A loop starts where the one before it finished, and the first loop of a
    /// region starts where the nozzle stood when the region opened.
    fn trace(&self, current: Loop, mut visit: impl FnMut((f64, f64), (f64, f64), Option<Arc>)) {
        let mut from = match current.lead {
            0 => self.entry,
            lead => self.buffer[lead - 1].at,
        };
        for index in current.lead..current.end {
            let buffered = self.buffer[index];
            if buffered.extrudes {
                visit(from, buffered.at, buffered.arc);
            }
            from = buffered.at;
        }
    }

    /// How much of a loop's path falls in each of the given sets, how much
    /// path there was, and the cells it went through, left in `path`.
    ///
    /// [`Trace::Refused`] where the walk could not be completed — a move no
    /// printer makes, which no answer can be read off. The counts that come
    /// back with it describe part of the loop and are not a share of it.
    fn shares(
        &self,
        sets: [Option<&Cells>; 5],
        current: Loop,
        path: &mut Vec<u32>,
    ) -> ([usize; 5], usize, Trace) {
        let mut found = [0usize; 5];
        let mut traced = Trace::Whole;
        path.clear();
        self.trace(current, |from, to, arc| {
            let walk = footprint::cells(footprint::Grid::default(), from, to, arc, |cell| {
                path.push(cell);
                for (at, set) in sets.iter().enumerate() {
                    found[at] += usize::from(set.is_some_and(|cells| cells.has(cell)));
                }
            });
            if walk == Trace::Refused {
                traced = Trace::Refused;
            }
        });
        (found, path.len(), traced)
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

    fn push(&mut self, line: &[u8]) -> io::Result<()> {
        write_line(&mut self.out, line)
    }
}

fn write_line<W: Write>(out: &mut W, line: &[u8]) -> io::Result<()> {
    out.write_all(line)?;
    out.write_all(b"\n")
}

/// The `M82`/`M83` that puts the extruder in the convention `absolute` names.
fn e_mode(absolute: bool) -> Code {
    if absolute {
        Code::AbsoluteE
    } else {
        Code::RelativeE
    }
}

#[cfg(test)]
mod tests;
