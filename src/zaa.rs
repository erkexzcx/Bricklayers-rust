//! Z anti-aliasing.
//!
//! A layer is flat and the model it came from usually is not, so wherever the
//! surface of a part is shallower than about 45° the print leaves a staircase:
//! each tread is one layer's worth of surface laid at one height, and each
//! riser is the full layer height. The treads are what catch the light.
//!
//! This follows the surface across each tread instead, varying the height of
//! the extrusion within the layer by up to half a layer either way, and
//! metering each stretch for the gap it actually crosses. The staircase
//! becomes a ramp, and the ramps of consecutive layers meet exactly: one ends
//! half a layer above its plane where the next begins half a layer below its
//! own.
//!
//! Where the surface is comes from [`surface`](crate::zaa::surface), which reads it
//! off the outlines of the layers either side rather than off the model — a
//! post-processor is handed G-code and nothing else.
//!
//! It runs over the top surface, the ironing above it, and the **walls**. The
//! walls matter more than they sound: a slope of more than about ten degrees
//! leaves a tread narrower than the wall stack standing on it, so there is no
//! top-surface region at all and the staircase is made entirely of wall. The
//! visible one is always followed; the hidden ones are followed only when
//! [`brick`](crate::brick) is not running in the same pass, because the bead
//! under a hidden wall may be one bricking raised, and lowering onto it would
//! close a gap that was metered open.
//!
//! It is written as a [`Write`] that sits in front of the real one, so the two
//! transforms compose in a single pass over the file: `brick` writes its
//! output into this, and this writes the finished G-code out.

use std::borrow::Cow;
use std::io::{self, BufRead, Write};

use crate::gcode::feature::{Feature, is_layer_marker};
use crate::gcode::{Code, Extruder, Line, MAX_LINE, Modal, repaired, write_e, write_fixed};
use crate::geometry::{Arc, Cells, Grid, turn};
use crate::scan::{FALLBACK_Z_FEEDRATE, Survey, ZAA_STAMP, is_a_height, is_stamp};
use crate::zaa::scout::Scout;
use crate::zaa::surface::{Builder, Field, MAX_WINDOW, Slice};

pub mod scout;
pub mod surface;

/// The shallowest slope worth following, in degrees.
///
/// The widest step this follows is that slope's own tread — the layer height
/// over the tangent — so it is derived per layer by [`reach_for`] rather than
/// given, and means the same slope on a 0.08 mm layer as on a 0.28 mm one.
///
/// A bound is needed, and not as a preference: a cell with something printed
/// over it measures a step as wide as the part, and fading that out on the
/// step's width against this is what stops it leaking a rise into the step
/// beside it. What tells a ledge or a flat top from a slope is the layer
/// *below*, not this — measured on a flat plate with a boss on it and on a
/// plain cube, both byte-identical at every reach up to 50 mm.
///
/// One degree is 11.5 mm of tread at 0.2 mm layers. Measured on real slices: a
/// 1.9° cone comes out untouched at 4 mm of reach and followed at 8 mm, and a
/// 60 mm spherical cap gives byte-identical output anywhere from 3 mm to
/// 50 mm, at the same time and the same peak memory. Being generous here
/// costs nothing and picks up the shallow tops that stair-step worst.
pub const SHALLOWEST_SLOPE: f64 = 1.0;

/// The widest step to follow on a layer of this height, in mm.
fn reach_for(height: f64) -> f64 {
    height / SHALLOWEST_SLOPE.to_radians().tan()
}

/// How far apart a move is sampled across the surface, as a share of the grid
/// the surface is measured on.
///
/// The rise is box-blurred over that grid, so it holds no feature narrower
/// than a cell and half a cell samples everything it can express. Measured on
/// a 60 mm cap against sampling at 0.02 mm: the written heights are identical
/// at the 99th percentile and 1.3 µm apart at the 99.9th, which is under the
/// 5 µm a move is simplified to. Only the sampling is this fine — consecutive
/// samples on one straight climb come out as a single move, because the
/// printer interpolates Z along a move already.
const STEP: f64 = 0.5;

/// Width a wall is assumed to have been metered at where the file states none,
/// in mm. Half of it is how far the traced centreline sits inside the outline
/// it came from.
const FALLBACK_WIDTH: f64 = 0.45;

/// How far a resampled arc may sit from the arc it replaces, in mm.
///
/// A chord of `2 * sqrt(SAG * (2 * radius - SAG))` is exactly this far from
/// the arc it spans, so an arc is sampled for its own curvature: a tight
/// radius gets a finer step than [`STEP`] and nothing is ever sampled coarser
/// than one. A micron is the grid a coordinate is written on, so the
/// resampling cannot be seen in the file.
///
/// It bounds the *writing* as well as the sampling, and that is the half that
/// is easy to lose: [`simplify`] judges a stretch by its climb, and every
/// sample of an arc at a steady height sits on one straight climb. Measured
/// on a 1000-wall Benchy before the span was passed to it, 74 arcs came out
/// as a single chord, the worst of them a 160° sweep of a 1.5 mm radius
/// straightened into a bar 1.27 mm inside its own wall.
const SAG: f64 = 0.001;

/// The longest chord that stays within [`SAG`] of an arc of this radius.
fn chord_of(radius: f64) -> f64 {
    2.0 * (SAG * (2.0 * radius - SAG)).max(0.0).sqrt()
}

/// How far a written height may sit from the surface it is following, in mm,
/// before the move is broken in two.
///
/// Five microns is under half of what the three-decimal Z word a printer is
/// given can even express a difference of, and far under what a Z axis
/// resolves. It bounds the move that is actually written, not a line the
/// simplifier merely proved could be drawn; see [`simplify`].
const TOLERANCE: f64 = 0.005;

/// Whether two heights reach the printer as the same command.
///
/// A Z word is written to three decimals, so two heights closer than a micron
/// are one height as far as the file is concerned. Comparing the full-precision
/// values instead writes a levelling move that commands the height the nozzle
/// already holds — a dead stop with a primed nozzle, which is the one thing
/// [`Pass::carrier`] exists to avoid. Measured on a real Benchy before this
/// was checked: 73 of 341 inserted height changes were exactly that.
fn same_height(a: f64, b: f64) -> bool {
    (a * 1000.0).round() == (b * 1000.0).round()
}

/// Points one move may be sampled at. A move longer than a bed is not a move,
/// and a corrupt coordinate must not turn into a million lines of output.
const MAX_SAMPLES: usize = 4096;

/// Lines held back between one bead and the next, so that a height change can
/// ride a move the slicer was already making instead of stopping the toolhead
/// for one of its own.
const TAIL: usize = 64;

/// What the transform is told. Everything else it needs — how wide a step to
/// follow and how finely to sample one — follows from the layer height and
/// from the grid the surface is measured on, so there is nothing to set.
#[derive(Clone, Copy, Debug, Default)]
pub struct Config {
    /// Layer height in mm, used for every layer. `None` takes each layer's own
    /// height from the file, which is the only right answer where the slicer
    /// varied it.
    pub layer_height: Option<f64>,
    /// Width the visible wall was metered at, in mm. `None` reads it off the
    /// file, and falls back to a stock profile where the file states none.
    pub wall_width: Option<f64>,
    /// Whether bricking is raising the hidden walls in this same run.
    ///
    /// It decides who owns them; see [`Pass::reshapes`]. The default is the
    /// safe answer, so a caller that forgets it gives up reach rather than
    /// correctness.
    pub bricked: bool,
}

/// What a rewrite did, for reporting.
#[derive(Clone, Copy, Debug, Default)]
pub struct Stats {
    /// Layers holding at least one move that was followed.
    pub layers: usize,
    /// Moves whose height was made to follow the surface.
    pub moves: usize,
    /// Moves written in their place. One straight climb is one move, however
    /// far it runs.
    pub segments: usize,
    /// Lowest and highest a surface was taken from its own plane, in mm.
    pub rise: Option<(f64, f64)>,
    /// Filament the output calls for on the surfaces this touched, in mm of
    /// stock, before it was re-metered.
    pub filament: f64,
    /// What re-metering those surfaces for the gaps they really cross added,
    /// in mm of stock. Negative where a surface came out below its plane.
    pub added: f64,
}

#[derive(Clone, Debug)]
pub struct Outcome {
    pub gcode: String,
    pub stats: Stats,
}

/// Rewrites a G-code stream, reading and writing a line at a time.
///
/// `lookahead` is a second reader over the same stream: the surface of a layer
/// is measured against the layer printed over it, which a single pass has not
/// reached yet. `survey` comes from an earlier pass; see [`Survey::read`].
pub fn stream<R: BufRead, S: BufRead, W: Write>(
    mut reader: R,
    lookahead: S,
    writer: W,
    config: &Config,
    survey: &Survey,
) -> io::Result<Stats> {
    let mut pass = Pass::new(writer, lookahead, config, survey);
    io::copy(&mut reader, &mut pass)?;
    pass.finish()
}

/// Rewrites G-code held in memory. Convenient for short inputs and tests; a
/// file goes through [`stream`] instead.
pub fn apply(source: &str, config: &Config) -> Outcome {
    let survey = Survey::of(source);
    let mut out = Vec::with_capacity(source.len() + source.len() / 4);
    let stats = stream(
        source.as_bytes(),
        source.as_bytes(),
        &mut out,
        config,
        &survey,
    )
    .expect("writing to a Vec cannot fail");

    Outcome {
        gcode: String::from_utf8(out).expect("rewritten G-code is UTF-8"),
        stats,
    }
}

/// A line held back, with the extrusion it asked for already resolved against
/// the input stream.
#[derive(Clone, Debug)]
struct Held {
    raw: Vec<u8>,
    e_span: Option<(usize, usize)>,
    e: Option<f64>,
    delta: Option<f64>,
    z: Option<f64>,
    f: Option<f64>,
    /// True where the line decides where the nozzle is next, so nothing after
    /// it could undo a height set on it.
    positions: bool,
    /// True where a height change can ride this line: a plain move, laying no
    /// bead, with the comment slot free for the stamp.
    carries: bool,
    /// True where the line already carries a comment, so riding it must not
    /// add a second one.
    stamped: bool,
    resets_origin: bool,
}

/// One move of the output: where it goes, how high, and what it extrudes.
type Segment = (f64, f64, f64, f64);

/// A sampled point of a move: where it is, how far along, and how far the
/// surface stands above the plane there.
type Sample = (f64, f64, f64, f64);

/// A bead that has been sampled but not yet written, so that the descent into
/// the bead after it can still be spread across its tail.
///
/// How fast a surface may fall is a property of the path, not of one move of
/// it. A bead ending where the next one runs under the layer above has to make
/// a start on the descent itself, because the next has no room left to make
/// it — and once written it could only be corrected by a `G1 Z` of its own,
/// which is a dead stop with a primed nozzle over no travel at all.
#[derive(Clone, Debug, Default)]
struct Pending {
    /// Empty where nothing is held; everything else here describes these.
    samples: Vec<Sample>,
    covered: Vec<bool>,
    /// The line the bead arrived as, kept for its comment and its feedrate.
    line: Vec<u8>,
    layer: usize,
    plane: f64,
    height: f64,
    span: f64,
    delta: f64,
    /// True where the bead lays nothing new, so it follows the surface without
    /// being re-metered for the gap it crosses.
    dry: bool,
}

pub struct Pass<W, R> {
    out: W,
    scout: Scout<R>,
    builder: Builder,
    field: Field,

    /// How finely the surface is measured, which is as finely as a print this
    /// wide can afford.
    grid: Grid,
    /// Half the width of the bead whose path traced the outlines.
    bead: f64,
    /// Whether bricking owns the hidden walls in this run; see
    /// [`Pass::reshapes`].
    bricked: bool,
    /// Layer height to use where the file measured none.
    height: f64,
    /// Height the file measured for each layer, empty unless the slicer varied
    /// them.
    heights: Vec<f64>,
    object_starts: Vec<usize>,
    layer_markers: bool,
    z_feedrate: f64,

    /// Bytes of the line still arriving.
    partial: Vec<u8>,
    /// True while a line already past what one may be held as is being copied
    /// through in pieces, so the rest of it is copied too rather than read.
    spilled: bool,
    extruder: Extruder,
    /// The positioning mode and units every coordinate is read in. Custom
    /// G-code wraps its moves in `G91` or `G20`, and a height written into one
    /// would not mean what it says.
    modal: Modal,
    feature: Feature,
    layer: usize,
    started: bool,
    /// Lowest height commanded since this layer's marker, which is the layer's
    /// own plane: a Z-hop and a raised wall both only ever lift.
    plane: Option<f64>,
    /// Height the stream arriving last put the nozzle at, which is not where
    /// this pass has left it. A wall standing on its plane is one bricking did
    /// not raise, and so one this may move.
    commanded: Option<f64>,
    nozzle_z: Option<f64>,
    feedrate: Option<f64>,
    at: (f64, f64),
    /// Layer [`Pass::field`] describes, so it is built once per layer and only
    /// for a layer that has a surface on it.
    built: Option<usize>,
    /// True once a layer whose outline could not be followed has been
    /// reported. Said once, rather than once per layer.
    warned: bool,
    tail: Vec<Held>,
    samples: Vec<Sample>,
    /// True where the sample of the same index has something printed over it,
    /// so [`Pass::ease`] leaves it exactly on the plane.
    covered: Vec<bool>,
    keep: Vec<usize>,
    plan: Vec<Segment>,
    /// The bead sampled last, held back until the one after it is known; see
    /// [`Pending`].
    pending: Pending,
    /// Height the first of the planned moves starts from, which is what the
    /// travel reaching it has to leave the nozzle at.
    entry: f64,
    /// True where the last line written laid a bead and nothing since has
    /// moved the nozzle, so a height change here would have to be a stop of
    /// its own between two extrusions.
    printing: bool,
    /// True where a surface was left standing off its own plane, so the next
    /// bead has to be put back on it. A file this transform never touches is
    /// never levelled either, and comes back exactly as it arrived.
    lifted: bool,
    /// Last layer counted into the statistics, so a layer with several
    /// followed regions counts once.
    counted: Option<usize>,
    stats: Stats,
}

impl<W: Write, R: BufRead> Pass<W, R> {
    pub fn new(out: W, lookahead: R, config: &Config, survey: &Survey) -> Self {
        let uniform = config.layer_height.filter(is_a_height);
        let grid = match survey.footprint {
            Some([left, front, right, back]) => {
                Grid::for_span(right - left, back - front, MAX_WINDOW)
            }
            None => Grid::default(),
        };
        // The visible wall is what traces the outline, so its width is what
        // says where the outline runs. The hidden walls' width stands in
        // where only that is stated, on the same reasoning bricking uses.
        let width = config
            .wall_width
            .or(survey.skin_width)
            .or(survey.wall_width)
            .filter(|width| width.is_finite() && *width > 0.0)
            .unwrap_or(FALLBACK_WIDTH);
        Self {
            out,
            scout: Scout::new(lookahead, grid),
            builder: Builder::default(),
            field: Field::default(),
            grid,
            bead: width / 2.0,
            bricked: config.bricked,
            height: uniform.unwrap_or(survey.layer_height),
            // A height given by the caller is the one they want used, so it
            // stands in for the measurement rather than beside it.
            heights: match uniform {
                Some(_) => Vec::new(),
                None => survey.layer_heights.clone(),
            },
            object_starts: survey.object_starts.clone(),
            layer_markers: survey.layer_markers,
            z_feedrate: survey.z_feedrate.unwrap_or(FALLBACK_Z_FEEDRATE),
            partial: Vec::new(),
            spilled: false,
            extruder: Extruder::new(),
            modal: Modal::new(),
            feature: Feature::Other,
            layer: 0,
            started: false,
            plane: None,
            commanded: None,
            nozzle_z: None,
            feedrate: None,
            at: (0.0, 0.0),
            built: None,
            warned: false,
            tail: Vec::new(),
            samples: Vec::new(),
            covered: Vec::new(),
            keep: Vec::new(),
            plan: Vec::new(),
            pending: Pending::default(),
            entry: 0.0,
            printing: false,
            lifted: false,
            counted: None,
            stats: Stats::default(),
        }
    }

    /// Writes out anything still held and reports what was done.
    pub fn finish(mut self) -> io::Result<Stats> {
        // A line copied through in pieces still owes its terminator even where
        // the last piece ended exactly on the buffer's end.
        if !self.partial.is_empty() || self.spilled {
            self.take()?;
        }
        self.release()?;
        let _ = self.drain(None, false)?;
        self.out.flush()?;
        Ok(self.stats)
    }

    /// Hands the line in `partial` to the transform and empties it, keeping
    /// what it allocated.
    fn take(&mut self) -> io::Result<()> {
        let mut buffer = std::mem::take(&mut self.partial);
        if buffer.last() == Some(&b'\r') {
            buffer.pop();
        }
        let outcome = self.consume(&buffer);
        buffer.clear();
        self.partial = buffer;
        outcome
    }

    /// Reads one whole line, or copies out the tail of one that was already
    /// being copied.
    fn consume(&mut self, buffer: &[u8]) -> io::Result<()> {
        // The rest of a line already partly written goes out the way the rest
        // of it did. Reading it would hand [`Line::parse`] a fragment short
        // enough to look like a command, and it is this end of the line that
        // the terminator finally belongs to.
        if std::mem::take(&mut self.spilled) {
            self.out.write_all(buffer)?;
            return self.out.write_all(b"\n");
        }
        // G-code is not guaranteed to be UTF-8: slicers copy object and
        // filament names into comments in whatever the host uses. The repair
        // is only what the parser reads; the bytes themselves are what gets
        // written back.
        match repaired(buffer) {
            Cow::Borrowed(text) => self.feed(text, buffer),
            Cow::Owned(text) => self.feed(&text, buffer),
        }
    }

    /// Copies out what has arrived of a line already too long to be held,
    /// exactly as it arrived and with no terminator behind it.
    ///
    /// Whatever is held back goes first: the piece is copied *past* this
    /// transform rather than through it, so it must not overtake the beads
    /// read before it.
    fn spill(&mut self) -> io::Result<()> {
        self.release()?;
        let _ = self.drain(None, false)?;
        let mut piece = std::mem::take(&mut self.partial);
        let outcome = self.out.write_all(&piece);
        piece.clear();
        self.partial = piece;
        self.spilled = true;
        outcome
    }

    fn feed(&mut self, raw: &str, bytes: &[u8]) -> io::Result<()> {
        let line = Line::parse_bytes(raw, bytes);

        if let Some(text) = line.marker() {
            if is_layer_marker(text) {
                self.layer += usize::from(std::mem::replace(&mut self.started, true));
                self.feature = Feature::Other;
                self.plane = None;
            } else if let Some(feature) = Feature::from_marker(text) {
                self.feature = feature;
            }
            return self.hold(line, None);
        }

        // Custom G-code — a colour change, an MMU swap, a timelapse or a
        // layer-change script — wraps its moves in `G91` or `G20`. Whatever is
        // held back is written out while a plain height still means what it
        // says, and the nozzle put back on the plane the file expects it on.
        if matches!(line.code, Code::RelativePosition | Code::Inches) && self.modal.is_plain() {
            self.release()?;
            let target = self.lifted.then(|| self.commanded.unwrap_or(self.plane()));
            let _ = self.drain(target, true)?;
        }
        // A number is not a height until the mode it is read in is known:
        // under `G91` a `G1 Z0.6` is a lift, and under `G20` it is 15.24 mm.
        let moved = self.modal.apply(&line);

        match line.code {
            Code::AbsoluteE | Code::RelativeE => self.extruder.set_mode(line.code),
            // A `G92` moves the origin rather than the filament. The reset
            // reaches the output stream only when the line is written, since
            // lines held back have not been written yet. Where it names an
            // axis it also moves the frame the next coordinate is read in, so
            // the position it states is taken as read.
            Code::SetPosition => {
                if let Some(e) = line.e {
                    self.extruder.observe_origin(e);
                }
                let (x, y, _) = self.modal.position();
                self.at = (x, y);
            }
            _ => {}
        }
        if line.z.is_some() && line.is_move() {
            let z = self.modal.position().2;
            self.plane = Some(self.plane.map_or(z, |had: f64| had.min(z)));
            self.commanded = Some(z);
        }

        // A slicer names only the axes that change, so a move starts wherever
        // the last one left off.
        let from = self.at;
        if let Some((x, y, _)) = moved {
            self.at = (x, y);
        }
        let delta = line
            .e
            .filter(|_| line.draws())
            .map(|e| self.extruder.observe(e));
        let lays = line.draws()
            && (line.x.is_some() || line.y.is_some())
            && delta.is_some_and(|delta| delta > 0.0);
        if !lays {
            return self.hold(line, delta);
        }
        self.bead(line, from, delta.unwrap_or(0.0))
    }

    /// Holds a line back for the next bead to ride, or writes out what is held
    /// and starts again where the tail has grown past its cap.
    fn hold(&mut self, line: Line<'_>, delta: Option<f64>) -> io::Result<()> {
        if self.tail.len() >= TAIL {
            // The bead held back was read before any of these, so it goes out
            // before them.
            self.release()?;
            let _ = self.drain(None, false)?;
        }
        self.tail.push(Held {
            raw: line.origin().to_vec(),
            e_span: line.e_span(),
            e: line.e,
            delta,
            // Where the move leaves the nozzle, not the number on the line:
            // every height compared against this one is absolute and in mm.
            z: line
                .z
                .filter(|_| line.is_move())
                .map(|_| self.modal.position().2),
            f: line.f,
            positions: line.is_move() && (line.is_xy_move() || line.z.is_some()),
            // A line already carrying one of this tool's own stamps can still
            // take a height: the comment slot is only needed for a stamp, and
            // there is already one there. Anything else keeps its comment.
            // A line read in relative mode or in inches can take none at all —
            // the tail outlives the section, so this is asked once, here,
            // while the mode it was read in is still known.
            carries: self.modal.is_plain()
                && line.is_move()
                && (line.is_xy_move() || line.z.is_some())
                && line.e.is_none()
                && line.comment().is_none_or(is_stamp),
            stamped: line.comment().is_some(),
            resets_origin: line.code == Code::SetPosition,
        });
        Ok(())
    }

    /// Writes one bead, following the surface under it where there is one.
    fn bead(&mut self, line: Line<'_>, from: (f64, f64), delta: f64) -> io::Result<()> {
        let plane = self.plane();
        let span = self.follow(from, &line, plane)?;
        let target = match (span.is_some(), self.lifted) {
            (true, _) => Some(self.entry),
            // Back where the arriving stream has this bead, which is its plane
            // unless another transform moved it. Levelling to the plane
            // regardless would undo a raise this one did not make: with the
            // hidden wall followed, the bead put back is the visible wall that
            // bricking raised, and it came down half a layer onto it.
            (false, true) => Some(self.commanded.unwrap_or(plane)),
            // Nothing here has moved the nozzle, so nothing here decides where
            // it goes.
            (false, false) => None,
        };
        // The bead still held back is the one that has to start the descent
        // into this one, and it can still be lowered.
        if let Some(z) = target {
            self.carry(z);
        }
        self.release()?;
        let ride = self.drain(target, span.is_some())?;
        match span {
            Some(span) => {
                self.stash(line.origin(), delta, plane, span);
                Ok(())
            }
            None => self.write_bead(line, delta, ride),
        }
    }

    /// Samples one bead across the surface, and reports how far one written
    /// move of it may run. `None` where the whole of it stays flat, which is
    /// the common case and has to cost nothing.
    fn follow(&mut self, from: (f64, f64), line: &Line<'_>, plane: f64) -> io::Result<Option<f64>> {
        // Without layer markers there are no layers to compare, so there is no
        // surface to measure.
        if !self.layer_markers || !self.reshapes(plane) {
            return Ok(None);
        }
        let to = self.at;
        self.build()?;
        if self.field.is_flat() {
            return Ok(None);
        }
        let height = self.height_at(self.layer);
        // The wall that shows is never commanded above its own plane. That is
        // bricking's invariant and it holds for the whole binary whichever
        // transforms are run: a bead of the visible wall standing proud is out
        // of reach of the nozzle's flat underside, so what would be ironed
        // level is free to bulge, and it does it where it shows. Where the
        // tread is wide its centreline sits below the plane anyway and the cap
        // never fires; where the tread is down to a bead the cap holds it
        // still and the surface beside it does the climbing.
        let ceiling = match self.feature {
            Feature::ExternalPerimeter => 0.0,
            _ => f64::INFINITY,
        };
        if !is_a_height(&height) {
            return Ok(None);
        }
        let arc = line.arc_between(from, to);
        let Some(span) = self.sample(from, to, arc, height, ceiling, plane) else {
            return Ok(None);
        };
        // What is written comes from these two, so a bead with no run to it is
        // refused here rather than coming out as no moves at all.
        let (Some(&(_, _, _, start)), Some(&(_, _, total, _))) =
            (self.samples.first(), self.samples.last())
        else {
            return Ok(None);
        };
        if !(total.is_finite() && total > 0.0) {
            return Ok(None);
        }
        self.entry = plane + start;
        Ok(Some(span))
    }

    /// Holds the bead just sampled back until the one after it is known, and
    /// records where it will leave the nozzle.
    fn stash(&mut self, raw: &[u8], delta: f64, plane: f64, span: f64) {
        std::mem::swap(&mut self.pending.samples, &mut self.samples);
        std::mem::swap(&mut self.pending.covered, &mut self.covered);
        self.pending.line.clear();
        self.pending.line.extend_from_slice(raw);
        self.pending.layer = self.layer;
        self.pending.plane = plane;
        self.pending.height = self.height_at(self.layer);
        self.pending.span = span;
        self.pending.delta = delta;
        // Ironing is a second pass over a surface that is already there, so it
        // has to follow it in Z, and it is deliberately not metered for the
        // gap it crosses.
        self.pending.dry = self.feature == Feature::Ironing;
        self.leaves();
    }

    /// Writes the bead held back, if there is one.
    fn release(&mut self) -> io::Result<()> {
        if self.pending.samples.is_empty() {
            return Ok(());
        }
        let pending = std::mem::take(&mut self.pending);
        self.build_plan(&pending);
        let written = self.write_plan(&pending);
        self.pending = pending;
        self.pending.samples.clear();
        self.pending.covered.clear();
        written
    }

    /// Records where the bead held back will leave the nozzle, so what is
    /// written between now and then is written against its end rather than
    /// against the bead before it.
    fn leaves(&mut self) {
        let Some(&(_, _, _, rise)) = self.pending.samples.last() else {
            return;
        };
        let z = (self.pending.plane + rise).max(0.0);
        self.nozzle_z = Some(z);
        self.lifted = z != self.pending.plane;
    }

    /// True where nothing since the bead held back moves the nozzle, so it and
    /// the bead arriving are one continuous path with no travel between them
    /// for a height change to ride.
    fn joined(&self) -> bool {
        !self.tail.iter().any(|held| held.positions)
    }

    /// Spreads the descent into whatever comes next across the tail of the
    /// bead still held back.
    ///
    /// [`Pass::ease`] holds the limit inside one move, and a bead ends exactly
    /// where the next one begins: a height the next one has to start from is a
    /// ceiling on this one's last sample, and at one layer height per bead
    /// width back from it on the samples before that. Without this the whole
    /// step lands on the join, where there is no travel to make it over —
    /// measured on a 60 mm cap at 0.05 mm cells, a bead reaching a covered
    /// edge 0.10 mm in was clipped from 0.100 to 0.044 mm while the bead
    /// before it had already been written ending at 0.100.
    ///
    /// Like `ease` it only ever **lowers** a sample, and never touches a
    /// covered one, so a bead under the layer above still sits exactly on its
    /// plane.
    fn carry(&mut self, target: f64) {
        if self.pending.samples.is_empty() || !self.joined() {
            return;
        }
        let fall = self.pending.height / (self.bead * 2.0);
        if !(fall.is_finite() && fall > 0.0) {
            return;
        }
        let rise = target - self.pending.plane;
        let Some(&(_, _, total, _)) = self.pending.samples.last() else {
            return;
        };
        for index in (0..self.pending.samples.len()).rev() {
            let (_, _, along, had) = self.pending.samples[index];
            let ceiling = rise + fall * (total - along);
            // Further back the ceiling only rises, so nothing behind this is
            // above it either.
            if ceiling >= had {
                break;
            }
            if !self.pending.covered[index] {
                self.pending.samples[index].3 = ceiling;
            }
        }
        self.leaves();
    }

    /// True where a bead of this region, at this height, is one to reshape.
    ///
    /// The top surface and the ironing over it are always fair game: nothing
    /// is printed against them. The **visible** wall is too, and it has to be:
    /// a slope of more than about ten degrees leaves a tread narrower than the
    /// wall stack standing on it, so the staircase is made of wall and there
    /// is no top-surface region to follow at all.
    ///
    /// Two things make the visible wall safe. Bricking never raises it, so a
    /// bead of it standing on its own plane is a bead nothing else has moved —
    /// and it standing on its plane is exactly what is tested here. And what
    /// sits under it, on a slope, is the outermost hidden loop of the layer
    /// below, which is the loop bricking caps: the layer above does not cover
    /// it. So the gap under it is one whole layer, which is what it was
    /// metered for.
    ///
    /// A **hidden** wall is exposed too wherever the tread is wider than its
    /// own inset, and on a shallow slope that is most of what the eye sees:
    /// measured over the layers of a 60 mm spherical cap that leave a tread
    /// wider than a bead, the hidden walls carry **37% of the exposed path**,
    /// and following them took the smoothing from 0.084 of half a layer over
    /// 24% of that path to **0.301 over 49.8%**. So it is followed —
    /// but only where bricking is not running, because the two cannot both
    /// own it. The bead under a hidden wall is a hidden loop of the layer
    /// below, offset by the tread rather than the one directly beneath, and
    /// bricking may have raised that loop half a layer while [`Field::is_open`]
    /// says nothing about it. Lowering onto one would close a gap the slicer
    /// metered open, which is the blob capping exists to prevent.
    ///
    /// Nothing at all is reshaped while positioning is relative or units are
    /// inches: a bead written there is custom G-code, not a surface, and a
    /// height put on it would be read as a displacement or as an inch.
    fn reshapes(&self, plane: f64) -> bool {
        if !self.modal.is_plain() {
            return false;
        }
        match self.feature {
            Feature::TopSurface | Feature::Ironing => true,
            Feature::ExternalPerimeter => self.commanded == Some(plane),
            Feature::InternalPerimeter => !self.bricked && self.commanded == Some(plane),
            _ => false,
        }
    }

    /// Measures the surface of the layer being written, once per layer and
    /// only for a layer that turns out to have one.
    fn build(&mut self) -> io::Result<()> {
        if self.built == Some(self.layer) {
            return Ok(());
        }
        self.built = Some(self.layer);
        // A file that prints its objects one at a time builds each from the
        // bed up, so the layer under an object's first is another object's
        // and says nothing about this one's surface.
        let opens = self.object_starts.contains(&self.layer);
        let next_opens = self.object_starts.contains(&(self.layer + 1));
        // A step is a tread of the staircase seen from above, so how wide one
        // may be before it stops being a slope follows from this layer's own
        // height — which an adaptive slice changes every layer.
        let reach = reach_for(self.height_at(self.layer));
        let [below, here, above] = self.scout.around(self.layer)?;
        let nothing = Cells::on(self.grid);
        // An outline with a move missing from it is not a smaller outline: a
        // strip is measured as the distance from one to the next, so a gap in
        // any of the three puts the surface somewhere the part is not. The
        // layer is left exactly as the slicer wrote it — an empty `here` makes
        // the field flat, and a flat field is followed nowhere.
        let unread = [below, here, above]
            .iter()
            .flatten()
            .any(|cells| cells.refused() > 0);
        let here = here.filter(|_| !unread);
        let slice = Slice {
            here: here.unwrap_or(&nothing),
            above: above.filter(|_| here.is_some() && !next_opens),
            below: below.filter(|_| here.is_some() && !opens),
            reach,
            bead: self.bead,
        };
        self.builder.build(&mut self.field, slice);
        if unread {
            self.warn_about_the_trace();
        }
        Ok(())
    }

    /// Says once that a layer's outline could not be followed. The user's
    /// print is already running, so this is never a failure: the layers it
    /// touches keep the heights the slicer gave them.
    fn warn_about_the_trace(&mut self) {
        if std::mem::replace(&mut self.warned, true) {
            return;
        }
        eprintln!(
            "corbel: warning: a move could not be followed, so the layers holding it keep the heights the slicer gave them"
        );
    }

    /// Walks a move, recording where the surface stands at each step.
    ///
    /// Returns how far one written move may run: an arc is replaced by the
    /// chords through its samples, so no chord of it may span more than
    /// [`chord_of`] its radius, however flat the climb across it is. `None`
    /// where nothing it would command differs from the height the bead already
    /// has, which is the common case and has to cost nothing.
    fn sample(
        &mut self,
        from: (f64, f64),
        to: (f64, f64),
        arc: Option<Arc>,
        height: f64,
        ceiling: f64,
        plane: f64,
    ) -> Option<f64> {
        self.samples.clear();
        self.covered.clear();
        let curve = arc.and_then(|arc| turn(from, to, arc));
        let length = match curve {
            Some((_, radius, _, sweep)) => radius * sweep.abs(),
            None => (to.0 - from.0).hypot(to.1 - from.1),
        };
        if !length.is_finite() || length <= 0.0 {
            return None;
        }
        // An arc is replaced by the chords through its samples, so a tight one
        // is sampled for its own curvature rather than at the grid's step.
        let along_grid = self.grid.cell() * STEP;
        let (span, step) = match curve {
            Some((_, radius, _, _)) => {
                let chord = chord_of(radius);
                (chord, chord.min(along_grid))
            }
            None => (f64::INFINITY, along_grid),
        };
        // An arc shorter than one sampling step is written exactly as the
        // slicer wrote it. There is nothing to follow across it — the rise is
        // blurred over a whole cell, so it holds no feature that short — and
        // resampling it can only round it onto its own chord. It is also the
        // one radius the chord cannot describe: at or under half of [`SAG`]
        // `chord_of` is zero, `length / step` is then infinite, `as usize`
        // saturates, and a three-micron arc comes out as [`MAX_SAMPLES`]
        // straight moves whose X, Y and Z all round to the same three
        // decimals, each carrying a positive `E`.
        if curve.is_some() && length <= along_grid {
            return None;
        }
        let steps = ((length / step).ceil() as usize).clamp(1, MAX_SAMPLES);

        for step in 0..=steps {
            let along = length * step as f64 / steps as f64;
            let (x, y) = match curve {
                // The last sample is the point the file commanded. Rebuilding
                // it from the radius instead lands on the I/J circle, and the
                // end of a `G2` is not always on its own circle — a slicer
                // rounds it to the micron, and nothing says a hand-edited or
                // lossily re-encoded one is anywhere near it. What the move
                // reaches would then be a point the file never asked for, and
                // the move after it would start from somewhere the nozzle is
                // not.
                _ if step == steps => to,
                Some((centre, radius, start, sweep)) => {
                    let angle = start + sweep * step as f64 / steps as f64;
                    (
                        centre.0 + radius * angle.cos(),
                        centre.1 + radius * angle.sin(),
                    )
                }
                None => (
                    from.0 + (to.0 - from.0) * step as f64 / steps as f64,
                    from.1 + (to.1 - from.1) * step as f64 / steps as f64,
                ),
            };
            // A step that runs under something printed on it is laid against
            // that, so it goes back to the plane there.
            let open = self.field.is_open(x, y);
            let rise = match open {
                true => (self.field.at(x, y) * height).min(ceiling),
                false => 0.0,
            };
            self.covered.push(!open);
            self.samples.push((x, y, along, rise));
        }
        // A bead begins exactly where the one before it ended, so the limit on
        // how fast a surface may fall carries across the join: this one may
        // not start above where the bead still held back leaves the nozzle.
        let entry = match self.joined() {
            true => self.pending.samples.last().map(|sample| sample.3),
            false => None,
        };
        self.ease(height, entry);
        // A rise the `Z` word cannot express is not a rise. The field is
        // quantised to a fraction of a layer and then read between cells, so
        // values well under the micron a height is written to are ordinary
        // wherever the rise passes through zero — and a bead there would be
        // taken apart and metered again to command the height it already has.
        // An arc is the expensive case: it is discarded and rewritten as the
        // straight chords through its samples, every one of them on the plane.
        let moved = self
            .samples
            .iter()
            .any(|&(_, _, _, rise)| !same_height(plane + rise, plane));
        moved.then_some(span)
    }

    /// Brings a stretch back onto the plane before it runs under something,
    /// rather than in one step at the edge of it.
    ///
    /// The surface reaches its highest exactly where the layer above begins —
    /// that is what makes one layer's ramp meet the next one's — and a bead
    /// that carries on under the layer above has to be back on the plane,
    /// because anything left standing proud there starves the bead laid on it.
    /// Between the two is a drop of half a layer in the width of one cell,
    /// which is not a path a nozzle can take: measured on a Benchy, 428 of
    /// 3042 written moves came out steeper than one in two, and 530 of them
    /// changed height over less than 0.05 mm of travel.
    ///
    /// So the descent is spread out ahead of the edge instead, at no more than
    /// one layer height per bead width — the steepest a surface can fall and
    /// still be a slope rather than a wall. It only ever **lowers** a sample,
    /// and never touches a covered one, so a bead under the layer above still
    /// sits exactly on its plane and the wall that shows keeps its ceiling.
    ///
    /// A **climb** is held to a different figure, because nothing is in its
    /// way. What bounds a descent is the nozzle's own flat underside plowing
    /// back through material it laid a bead width ago; climbing, it lifts away
    /// from that material and the gap it opens is one the extrusion is already
    /// metered for. The bound that is left is the surface itself: the rise is
    /// box-blurred over a grid cell, so the steepest the measured surface can
    /// ask for is one layer height per cell, and holding a climb to that never
    /// flattens anything the field can express. Held to the descent's figure
    /// instead it flattened a great deal: a bead leaving a covered stretch was
    /// kept low for a further bead width, and the far edge of a strip is
    /// exactly where the ramp has to reach half a layer for one layer's ramp
    /// to meet the next one's — so a tread narrower than a bead was levelled
    /// outright and the staircase it was there to remove came back.
    ///
    /// `entry` is where the bead before this one leaves the nozzle, which is
    /// this one's first sample seen from the other side of the join. Its
    /// counterpart is [`Pass::carry`], which takes the descent the other way.
    fn ease(&mut self, height: f64, entry: Option<f64>) {
        let fall = height / (self.bead * 2.0);
        if !(fall.is_finite() && fall > 0.0) {
            return;
        }
        let climb = height / self.grid.cell();
        if let (Some(entry), Some(first)) = (entry, self.samples.first_mut())
            && !self.covered[0]
            && first.3 > entry
        {
            first.3 = entry;
        }
        for index in (0..self.samples.len().saturating_sub(1)).rev() {
            let (_, _, ahead, above) = self.samples[index + 1];
            let (_, _, along, rise) = self.samples[index];
            let ceiling = above + fall * (ahead - along);
            if !self.covered[index] && rise > ceiling {
                self.samples[index].3 = ceiling;
            }
        }
        for index in 1..self.samples.len() {
            let (_, _, behind, under) = self.samples[index - 1];
            let (_, _, along, rise) = self.samples[index];
            let ceiling = under + climb * (along - behind);
            if !self.covered[index] && rise > ceiling {
                self.samples[index].3 = ceiling;
            }
        }
    }

    /// Turns the samples into the fewest moves that follow them.
    ///
    /// A printer interpolates Z along a move, so a straight climb is one move
    /// however long it runs and a curve costs one move per bend. Each move is
    /// metered for the gap its own stretch crosses: the layer under a surface
    /// is flat, so that gap is the layer height plus however far this stretch
    /// stands above the plane.
    ///
    /// It is metered over the stretch's **horizontal** run, because the gap it
    /// fills is measured vertically. The material lies between a flat layer
    /// below and the tilted top written here, so its volume is the bead's
    /// width times that gap integrated over the ground it covers — which is
    /// what the slicer's own rate, filament per mm of travel in the plane,
    /// already states. Metering a slanted piece for the longer path the nozzle
    /// takes instead pours in a further `1 / cos` of stock with nowhere to go,
    /// nine percent of it at the steepest grade [`Pass::ease`] allows, and
    /// breaks the one exact identity here: the pieces of a stretch that is not
    /// re-metered sum to what the slicer wrote for it.
    fn build_plan(&mut self, pending: &Pending) {
        self.plan.clear();
        let Some(&(_, _, total, _)) = pending.samples.last() else {
            return;
        };
        if !total.is_finite() || total <= 0.0 {
            return;
        }
        let rate = pending.delta / total;
        simplify(&pending.samples, TOLERANCE, pending.span, &mut self.keep);

        let mut anchor = 0usize;
        for index in 0..self.keep.len() {
            let at = self.keep[index];
            let (x, y, along, rise) = pending.samples[at];
            let run = along - pending.samples[anchor].2;
            if run <= 0.0 {
                continue;
            }
            let middle = (rise + pending.samples[anchor].3) / 2.0;
            let factor = match pending.dry {
                true => 1.0,
                false => (pending.height + middle) / pending.height,
            };
            let stock = (rate * run * factor).max(0.0);
            self.plan
                .push((x, y, (pending.plane + rise).max(0.0), stock));
            anchor = at;
        }
    }

    /// Writes the planned moves in place of the one that was read.
    fn write_plan(&mut self, pending: &Pending) -> io::Result<()> {
        let text = repaired(&pending.line);
        let line = Line::parse_bytes(&text, &pending.line);
        let comment = line.comment();
        let rate = line.f;
        let plane = pending.plane;
        let count = self.plan.len();
        let mut stock = 0.0;
        for index in 0..count {
            let (x, y, z, e) = self.plan[index];
            let value = self.extruder.advance(e);
            stock += e;
            self.nozzle_z = Some(z);
            let low = z - plane;
            self.stats.rise = Some(match self.stats.rise {
                Some((least, most)) => (least.min(low), most.max(low)),
                None => (low, low),
            });

            let out = &mut self.out;
            out.write_all(b"G1 X")?;
            write_fixed(out, x, 3)?;
            out.write_all(b" Y")?;
            write_fixed(out, y, 3)?;
            out.write_all(b" Z")?;
            write_fixed(out, z, 3)?;
            out.write_all(b" E")?;
            write_fixed(out, value, 5)?;
            if index == 0 {
                if let Some(rate) = rate {
                    write!(out, " F{rate}")?;
                }
                write!(out, " ; {ZAA_STAMP}surface")?;
            }
            // The comment the move arrived with rides its last piece, or the
            // stamp's own line where there is only one.
            if index + 1 == count
                && let Some(comment) = comment
            {
                match index {
                    0 => write!(out, " {}", comment.trim())?,
                    _ => write!(out, " ;{comment}")?,
                }
            }
            out.write_all(b"\n")?;
        }
        if let Some(rate) = rate {
            self.feedrate = Some(rate);
        }

        self.stats.moves += 1;
        self.stats.segments += count;
        self.stats.filament += pending.delta;
        self.stats.added += stock - pending.delta;
        self.lifted = self.nozzle_z != Some(plane);
        self.printing = true;
        if self.counted != Some(pending.layer) {
            self.counted = Some(pending.layer);
            self.stats.layers += 1;
        }
        Ok(())
    }

    /// Writes a bead that stays on its plane, with its extrusion carried
    /// forward through whatever the surfaces before it were re-metered by.
    ///
    /// `ride` is a height the bead has to take itself, set where the nozzle is
    /// coming straight off another bead and a move of its own would stop it
    /// dead; see [`Pass::level`].
    fn write_bead(&mut self, line: Line<'_>, delta: f64, ride: Option<f64>) -> io::Result<()> {
        if let Some(rate) = line.f {
            self.feedrate = Some(rate);
        }
        if line.z.is_some() && line.is_move() {
            self.nozzle_z = Some(self.modal.position().2);
            self.lifted = false;
        }
        let value = self.extruder.advance(delta);
        let written = match ride {
            Some(z) => self.write_ridden(&line, value, z),
            None if line.e == Some(value) => write_line(&mut self.out, line.origin()),
            None => {
                line.write_e(&mut self.out, value)?;
                self.out.write_all(b"\n")
            }
        };
        self.printing = true;
        written
    }

    /// Writes a bead with a `Z` word set on it, so that a height change
    /// between two extrusions is made along the bead rather than by a stop of
    /// its own before it.
    ///
    /// A line naming only one of `X` and `Y` cannot have both words edited at
    /// once, so the extrusion is settled first and the height is set on what
    /// that produced.
    fn write_ridden(&mut self, line: &Line<'_>, e: f64, z: f64) -> io::Result<()> {
        self.nozzle_z = Some(z);
        self.lifted = false;
        let stamped = line.comment().is_some();
        if line.e == Some(e) {
            line.write_z(&mut self.out, z)?;
        } else {
            let mut settled = Vec::new();
            line.write_e(&mut settled, e)?;
            let settled = String::from_utf8_lossy(&settled).into_owned();
            Line::parse(&settled).write_z(&mut self.out, z)?;
        }
        match stamped {
            true => self.out.write_all(b"\n"),
            false => writeln!(self.out, " ; {ZAA_STAMP}level"),
        }
    }

    /// Writes everything held back, leaving the nozzle at `target` by way of
    /// the last move that can take it there.
    ///
    /// `commands` says the bead about to be written sets a height on every
    /// move of its own, so it can be left to do it. The height returned is one
    /// that bead has to carry instead, where it has no plan of its own and the
    /// nozzle is coming straight off another bead.
    fn drain(&mut self, target: Option<f64>, commands: bool) -> io::Result<Option<f64>> {
        let carrier = target.and_then(|z| self.carrier(z));
        let mut held = std::mem::take(&mut self.tail);
        for (index, line) in held.iter().enumerate() {
            match carrier {
                Some(at) if at == index => {
                    let z = target.expect("a carrier is only chosen for a target");
                    self.ride(line, z)?;
                }
                _ => self.replay(line)?,
            }
        }
        held.clear();
        self.tail = held;
        match target {
            Some(z) => self.level(z, commands),
            None => Ok(None),
        }
    }

    /// The held line that can take the nozzle to `z` on its way, or `None`
    /// where one of its own is needed.
    ///
    /// The last move of the tail is the one to use, since anything after it
    /// would override the height. It has to be a plain move: an extrusion or a
    /// wipe follows the layer below and cannot be tilted, and a line that
    /// already carries a comment has no room for the stamp that stops the file
    /// being processed twice.
    ///
    /// Nor can a line read in relative mode or in inches carry one: the `Z`
    /// word put on it would be a displacement or an inch, not the place meant.
    fn carrier(&self, z: f64) -> Option<usize> {
        if !self.modal.is_plain() {
            return None;
        }
        let landing = self
            .tail
            .iter()
            .rev()
            .find_map(|held| held.z)
            .or(self.nozzle_z);
        if landing.is_some_and(|had| same_height(had, z)) {
            return None;
        }
        let index = self.tail.len() - self.tail.iter().rev().position(|h| h.positions)? - 1;
        let carrier = &self.tail[index];
        // Never ride a move the slicer put above the plane on purpose: pulling
        // a Z-hop down to printing height would drag the nozzle through what
        // it was lifted to clear.
        (carrier.carries && carrier.z.is_none_or(|had| had <= z)).then_some(index)
    }

    /// Replays a held line with its height set to `z`.
    fn ride(&mut self, held: &Held, z: f64) -> io::Result<()> {
        self.nozzle_z = Some(z);
        self.lifted = false;
        self.printing = false;
        if let Some(rate) = held.f {
            self.feedrate = Some(rate);
        }
        // No `E` word to carry forward: a line that carries a height has none.
        let text = repaired(&held.raw);
        Line::parse_bytes(&text, &held.raw).write_z(&mut self.out, z)?;
        match held.stamped {
            true => self.out.write_all(b"\n"),
            false => writeln!(self.out, " ; {ZAA_STAMP}level"),
        }
    }

    /// Replays a held line as it arrived, its extrusion carried forward.
    fn replay(&mut self, held: &Held) -> io::Result<()> {
        if let Some(z) = held.z {
            self.nozzle_z = Some(z);
            self.lifted = false;
        }
        if held.positions {
            self.printing = false;
        }
        if let Some(rate) = held.f {
            self.feedrate = Some(rate);
        }
        if held.resets_origin {
            if let Some(value) = held.e {
                self.extruder.advance_origin(value);
            }
            return write_line(&mut self.out, &held.raw);
        }
        let Some(delta) = held.delta else {
            return write_line(&mut self.out, &held.raw);
        };
        let value = self.extruder.advance(delta);
        // Whether the line has to be rewritten is whether the value it should
        // now carry differs from the one it was written with.
        if held.e == Some(value) {
            return write_line(&mut self.out, &held.raw);
        }
        write_e(&mut self.out, &held.raw, held.e_span, value)?;
        self.out.write_all(b"\n")
    }

    /// Puts the nozzle at `z` with a move of its own, where nothing held back
    /// could take it there — and never between two beads of one continuous
    /// path, where there is nothing held back at all.
    ///
    /// A `G1 Z` names no other axis, so the planner brings the toolhead to a
    /// dead stop to run it. On a seam with a primed nozzle that is the stop
    /// riding a move exists to avoid, and the height change is made over no
    /// travel whatever — infinitely steeper than the slope [`Pass::ease`] and
    /// [`Pass::carry`] hold the surface to. So it is not a fallback there: the
    /// bead about to be written takes the height instead, either on the moves
    /// its own plan commands (`commands`) or, where it has no plan, as a `Z`
    /// word on the move itself, which is what the returned height is for.
    ///
    /// While positioning is relative or units are inches nothing is written at
    /// all. `G1 Z{z}` there is a displacement or an inch, and the nozzle is
    /// where the file's own custom G-code put it. What is owed is settled at
    /// the first bead after the section, which is where the nozzle is needed
    /// back on its plane anyway.
    fn level(&mut self, z: f64, commands: bool) -> io::Result<Option<f64>> {
        if !self.modal.is_plain() {
            return Ok(None);
        }
        if self.nozzle_z.is_some_and(|current| same_height(current, z)) {
            self.lifted = false;
            return Ok(None);
        }
        if self.printing {
            return Ok((!commands).then_some(z));
        }
        self.nozzle_z = Some(z);
        self.lifted = false;
        let rate = self.z_feedrate;
        writeln!(self.out, "G1 Z{z:.3} F{rate} ; {ZAA_STAMP}level")?;
        match self.feedrate {
            Some(previous) if previous != rate => {
                writeln!(self.out, "G1 F{previous} ; {ZAA_STAMP}resume")?;
            }
            _ => {}
        }
        Ok(None)
    }

    /// The plane this layer is printed at.
    fn plane(&self) -> f64 {
        self.plane.or(self.nozzle_z).unwrap_or(0.0)
    }

    /// What `layer` was sliced at, falling back to the one height that
    /// describes a file the slicer did not vary.
    fn height_at(&self, layer: usize) -> f64 {
        self.heights
            .get(layer)
            .copied()
            .filter(is_a_height)
            .unwrap_or(self.height)
    }
}

impl<W: Write, R: BufRead> Write for Pass<W, R> {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        let mut from = 0;
        while let Some(at) = data[from..].iter().position(|byte| *byte == b'\n') {
            let end = from + at;
            self.partial.extend_from_slice(&data[from..end]);
            self.take()?;
            from = end + 1;
        }
        self.partial.extend_from_slice(&data[from..]);
        // A line with no end in sight is not held whole and is not read at
        // all: past the cap it is copied straight out, in pieces, exactly as
        // it arrived. Nothing this long is a command.
        if self.partial.len() >= MAX_LINE {
            self.spill()?;
        }
        Ok(data.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.out.flush()
    }
}

/// Keeps the fewest samples whose straight climbs stay within `tolerance` of
/// every sample between them, and which never run more than `span` apart.
///
/// One pass, by narrowing the range of slopes a segment could still take as
/// each new sample arrives: a slope that satisfies every sample so far exists
/// exactly while that range is not empty. A straight climb never empties it,
/// however many samples it holds, which is what turns a ramp into one move.
///
/// The corridor is kept to **half** the tolerance, because the line that gets
/// printed is not the slope the corridor was kept for. What is written is the
/// chord from the anchor to the last sample that fitted, and that chord's own
/// slope can sit a whole corridor away from a slope the interior samples were
/// tested against — an interior sample is one corridor from that slope and
/// never further along than the sample the chord ends at, so it is at most two
/// corridors from the chord itself. Two halves are what make the printed line,
/// rather than a line nobody prints, the thing `tolerance` describes.
///
/// `span` is what keeps that from straightening a **curve**. The samples of an
/// arc lie on one straight climb as readily as those of a straight move, and
/// the slope range says nothing about where they sit in the plane, so without
/// a bound on the run a whole arc collapses onto its own chord.
fn simplify(samples: &[Sample], tolerance: f64, span: f64, keep: &mut Vec<usize>) {
    keep.clear();
    if samples.len() < 2 {
        return;
    }
    let mut anchor = 0usize;
    let (mut least, mut most) = (f64::NEG_INFINITY, f64::INFINITY);
    let corridor = tolerance / 2.0;
    let bounds = |anchor: usize, index: usize| {
        let run = samples[index].2 - samples[anchor].2;
        let rise = samples[index].3 - samples[anchor].3;
        (run > 0.0).then(|| ((rise - corridor) / run, (rise + corridor) / run))
    };

    for index in 1..samples.len() {
        let stretched = index > anchor + 1 && samples[index].2 - samples[anchor].2 > span;
        let Some((low, high)) = bounds(anchor, index) else {
            continue;
        };
        let (low, high) = (least.max(low), most.min(high));
        if !stretched && low <= high {
            (least, most) = (low, high);
            continue;
        }
        // No straight climb from the anchor covers this sample as well as the
        // ones before it, so the last one that worked becomes the next anchor.
        keep.push(index - 1);
        anchor = index - 1;
        (least, most) = bounds(anchor, index).unwrap_or((f64::NEG_INFINITY, f64::INFINITY));
    }
    keep.push(samples.len() - 1);
}

fn write_line<W: Write>(out: &mut W, line: &[u8]) -> io::Result<()> {
    out.write_all(line)?;
    out.write_all(b"\n")
}

#[cfg(test)]
mod tests;
