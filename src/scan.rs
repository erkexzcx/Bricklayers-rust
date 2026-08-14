//! A single pre-pass that collects everything the transform needs to know
//! about a file before rewriting it.
//!
//! Nothing here keeps the G-code itself, only a handful of counters, so the
//! pass runs over a stream of any length.

use std::io::{self, BufRead};

use crate::gcode::feature::{Feature, is_layer_marker, unrecognised_region};
use crate::gcode::{Code, Extruder, Line, Lines, MAX_LINE, Modal};
use crate::geometry::Cells;
use crate::geometry::footprint::extent;
use crate::slicer::{self, WallOrder};

/// Layer height assumed when the file says nothing useful.
pub const FALLBACK_LAYER_HEIGHT: f64 = 0.2;

/// How far two layers' heights may differ and still count as the same, in mm.
///
/// Subtracting one printed Z from another leaves float noise, so a file sliced
/// at a fixed height does not measure at exactly that height twice. Measured
/// over three real fixed-height slices the whole spread is 3.6e-15 mm, while an
/// adaptive slice of a Benchy spreads 0.038 mm — thirteen orders apart, so
/// anything in between separates them. A micron is also below what a Z axis can
/// resolve, so two layers this close are the same layer height in every sense
/// that reaches the print.
const SAME_HEIGHT: f64 = 0.001;

/// Feedrate for an inserted Z move when the file never shows one, in mm/min.
/// 12 mm/s is slow enough for any Z axis, including a delta moving all three
/// towers at once.
pub const FALLBACK_Z_FEEDRATE: f64 = 720.0;

/// Comment left on the lines this tool inserts. Repeating the transform
/// compounds it, so a run recognises its own earlier work by these.
pub const BRICK_STAMP: &str = "corbel brick ";

/// The same, for the lines [`zaa`](crate::zaa) writes. Contouring a surface
/// that has already been contoured would measure it against a plane it is no
/// longer on.
pub const ZAA_STAMP: &str = "corbel zaa ";

/// What the same two stamps read before this tool was renamed. A file a
/// release under the old name already processed carries these and nothing
/// else, and it is just as compounded by a second pass, so both spellings are
/// recognised for as long as such files exist.
const LEGACY_BRICK_STAMP: &str = "bricklayers brick ";
const LEGACY_ZAA_STAMP: &str = "bricklayers zaa ";

/// True where a comment is one this tool wrote, under either name.
///
/// A stamp is written in FRONT of whatever the line it rides already carried,
/// because a raise may ride a travel the slicer had already annotated — a
/// `;WIPE_START`, an object name — and appending behind that would put the
/// stamp inside someone else's text. So a stamped line's comment is the stamp
/// followed by however much of the slicer's own note came after it, and what
/// identifies it is what the comment STARTS with rather than the line carrying
/// nothing else. Everything past the first `;` is one comment as far as
/// [`Line::comment`](crate::gcode::Line::comment) is concerned, which is why
/// this compares a prefix and never the whole.
pub fn is_stamp(comment: &str) -> bool {
    let comment = comment.trim_start();
    [BRICK_STAMP, ZAA_STAMP, LEGACY_BRICK_STAMP, LEGACY_ZAA_STAMP]
        .iter()
        .any(|stamp| comment.starts_with(stamp))
}

/// Where a file carrying no layer-change marker changes layer.
///
/// The survey and the rewrite index the same per-layer sets by the same layer
/// number, so a boundary they disagree on is worse than no boundary at all:
/// every set is then consulted for the wrong layer. This is the one rule both
/// of them use.
///
/// A layer change is confirmed by the first bead laid off the plane the last
/// one sat on, never by the Z move that reached it. A Z-hop lifts and comes
/// back down before anything is extruded again, so at the next bead the nozzle
/// is back on the plane and nothing is counted; a real change does not come
/// back down. It is [`Scan`]'s own "a layer's floor is the lowest height that
/// layer commanded", read forward instead of at the layer's end. Testing the Z
/// move instead counted every hop as a layer, which walked the rewrite's layer
/// number away from the survey's for the rest of the file.
#[derive(Clone, Copy, Debug, Default)]
pub struct Markerless {
    plane: Option<f64>,
}

impl Markerless {
    /// True where a bead laid with the nozzle at `z` is the first of a layer.
    /// The file's very first bead opens its first layer.
    ///
    /// Deliberately not a commit: the caller may still be holding a region of
    /// the layer that is ending, which has to be written out at that layer's
    /// plane before the next one opens.
    pub fn opens_a_layer(&self, z: f64) -> bool {
        self.plane != Some(z)
    }

    /// Opens the layer whose beads sit at `z`.
    pub fn open(&mut self, z: f64) {
        self.plane = Some(z);
    }

    /// The plane the open layer's beads sit at, or `None` before its first
    /// bead.
    ///
    /// It is measured rather than accumulated: the Z that reaches a layer is
    /// commanded while the layer before it is still open, so the lowest height
    /// seen since the boundary belongs to the next layer, not to this one.
    pub fn plane(&self) -> Option<f64> {
        self.plane
    }
}

#[derive(Clone, Debug)]
pub struct Survey {
    /// Number of printed layers, used to find the first and last one.
    pub layers: usize,
    /// True when the file carries explicit layer-change markers.
    pub layer_markers: bool,
    pub layer_height: f64,
    /// True when the layer height came from the file rather than the fallback.
    pub layer_height_detected: bool,
    /// Measured height of each layer, indexed from zero.
    ///
    /// Empty unless the slicer actually varied the height, so a file sliced at
    /// one height is described by [`layer_height`](Self::layer_height) alone and
    /// takes exactly the path it did before this was measured. A layer the pass
    /// could not measure holds zero.
    pub layer_heights: Vec<f64>,
    /// Order the file says its walls were printed in, from the configuration
    /// slicers append to the G-code. It cannot be measured from the moves
    /// themselves, so a file processed by hand has no other source.
    pub wall_order: Option<WallOrder>,
    /// Width the external perimeter was metered at, in mm, from the file's own
    /// settings block. `None` where the file states none.
    pub skin_width: Option<f64>,
    /// Width the internal perimeters were metered at, in mm, from the file's
    /// own settings block.
    ///
    /// It is what sets how far apart the slicer laid neighbouring beads, and
    /// so how much of each bead sits in the corner between two of them, which
    /// is what the flow the walls are metered at is derived from.
    pub wall_width: Option<f64>,
    /// Nozzle diameter in mm, which is what a width stated as a percentage is
    /// a percentage of.
    pub nozzle: Option<f64>,
    /// Slowest feedrate the file itself uses to move Z alone, in mm/min.
    pub z_feedrate: Option<f64>,
    /// True when [`brick`](crate::brick) has already run over this file.
    pub bricked: bool,
    /// True when [`zaa`](crate::zaa) has already run over this file.
    pub contoured: bool,
    /// Extrusions inside internal perimeters emitted as `G2`/`G3` arcs, which
    /// pass through untouched however the loop around them is shifted.
    pub arc_extrusions: usize,
    /// Region markers the file carried that name a perimeter, in either marker
    /// dialect.
    ///
    /// Zero is what a file this tool recognises nothing in looks like — an
    /// unknown slicer, or one an earlier post-processor stripped the markers
    /// out of. Both transforms find their work through those markers, so a
    /// file with none is rewritten to no effect.
    pub perimeters: usize,
    /// Region markers whose label named nothing this tool knows, in either
    /// dialect.
    ///
    /// An unknown label is never an error: the region it opens is copied out
    /// exactly as the slicer wrote it, which is the right answer for anything
    /// neither transform owns. But it is how a whole unsupported dialect goes
    /// by in silence — every marker is found, read, and classified as
    /// [`Feature::Other`], and nothing anywhere says so. Counting them is what
    /// lets a run report it.
    pub unknown_regions: usize,
    /// The first of those labels, copied off the line it arrived on so it can
    /// be quoted after the file has gone by.
    pub unknown_region: Option<String>,
    /// Layer each object starts at, in print order, beginning with zero.
    ///
    /// A file sliced to complete individual objects builds each one from the
    /// bed up, so it holds several first and last layers rather than the one
    /// pair a layer-by-layer file has.
    pub object_starts: Vec<usize>,
    /// Last layer of each object that carries an internal perimeter.
    ///
    /// Measured, because it is almost never the object's last layer: a part is
    /// closed by solid infill printed over its walls, and Orca ends a file with
    /// a layer marker whose only extrusion is unlabelled. On six real slices
    /// the walls stopped one to five layers below the last.
    pub object_tops: Vec<usize>,
    /// Where each layer's internal perimeters run with nothing above them.
    ///
    /// A raised bead stands half a layer proud, so whatever the slicer lays
    /// over it at the next plane meets a gap half the size it was metered for.
    /// That is only harmless where the thing above is the same column, raised
    /// too. Everywhere else — a shoulder closed by a top surface, a feature
    /// that ends, the top of the part — the bead has to be laid flat instead.
    /// Indexed by layer, by [`Markerless`] where the file states no layers of
    /// its own.
    pub uncovered: Vec<Cells>,
    /// Where each layer's internal perimeters run with nothing beneath them.
    ///
    /// The mirror of [`Survey::uncovered`]. A column that begins partway up —
    /// the underside of a shelf, the roof of a bridged hole — has no seam
    /// under its first bead, so raising that bead by the full offset asks it
    /// to span a layer and a half of gap while the slicer metered it for one.
    /// Indexed the same way as [`Survey::uncovered`].
    pub unsupported: Vec<Cells>,
    /// The box the part's own extrusions cover, as `[left, front, right,
    /// back]` in mm. `None` where nothing was laid down.
    ///
    /// It is what [`zaa`](crate::zaa) sizes its grid against: how finely a
    /// surface can be measured is a question of how much bed there is to
    /// measure, not of how long the file is.
    pub footprint: Option<[f64; 4]>,
}

impl Survey {
    pub fn of(source: &str) -> Self {
        let mut scan = Scan::default();
        for raw in source.lines() {
            scan.feed(raw);
        }
        scan.finish()
    }

    /// Surveys a stream, reading it once and keeping none of it.
    pub fn read<R: BufRead>(reader: R) -> io::Result<Self> {
        let mut scan = Scan::default();
        let mut lines = Lines::new(reader);
        // True while a line too long to be held whole is arriving in pieces.
        // A piece is not a line: a command is a few dozen bytes and a marker
        // fewer, so nothing this long is either, and a fragment read as one
        // would be a command the file never carried.
        let mut spilling = false;
        while let Some(raw) = lines.next_line()? {
            if !spilling && raw.text.len() < MAX_LINE {
                scan.feed(raw.text);
                continue;
            }
            // Asking costs a copy — the text borrows the reader — so it is
            // only ever asked of a line near the cap, or of the first line
            // after one. The last piece of a long line answers yes as well,
            // since it was assembled out of what the read before it carried
            // over, so anything answering no is a line of its own.
            let text = raw.text.to_owned();
            spilling = lines.partial();
            if !spilling {
                scan.feed(&text);
            }
        }
        Ok(scan.finish())
    }

    /// Objects the file prints one after another.
    pub fn objects(&self) -> usize {
        self.object_starts.len()
    }

    /// True when the slicer varied the layer height across the file, so no one
    /// number describes it and every layer has to be raised by its own half.
    pub fn variable_layers(&self) -> bool {
        !self.layer_heights.is_empty()
    }

    /// True when `layer` is the first of its object, whose raised loops span
    /// from the bed rather than from the layer below.
    pub fn opens_an_object(&self, layer: usize) -> bool {
        self.object_starts.contains(&layer)
    }

    /// True when `layer` tops an object's walls, so its loops have nothing
    /// above them to interlock with.
    ///
    /// This is the last layer holding an internal perimeter rather than the
    /// object's last layer: the two are rarely the same, and testing the layer
    /// count instead left every real file's topmost wall uncapped.
    pub fn closes_an_object(&self, layer: usize) -> bool {
        self.object_tops.contains(&layer)
    }

    /// Where `layer`'s walls have nothing above them, or `None` where the file
    /// gave the survey no way to tell.
    pub fn uncovered(&self, layer: usize) -> Option<&Cells> {
        self.uncovered.get(layer).filter(|cells| !cells.is_empty())
    }

    /// Where `layer`'s walls have nothing beneath them, or `None` where the
    /// file gave the survey no way to tell.
    pub fn unsupported(&self, layer: usize) -> Option<&Cells> {
        self.unsupported
            .get(layer)
            .filter(|cells| !cells.is_empty())
    }
}

/// True for a region that covers a raise without having to give it back.
///
/// A bead left standing half a layer proud is a step, and anything printed
/// over that step has to be metered for the gap the step left rather than for
/// a whole layer — which is why a column under solid infill, a top surface or
/// ironing is capped: those come out exactly as the slicer metered them.
///
/// These do not. Every one of them is a region [`brick`](crate::brick) buffers
/// as loops and meters against what the layer below actually left under it, so
/// a raise beneath one is already accounted for and capping it would throw
/// away the stagger for nothing. The visible wall and an overhang are the same
/// wall as the hidden loops — a slicer relabels a loop mid-path where it runs
/// out over air — and a thin wall is what that wall becomes where it narrows
/// to less than two beads, laid on its own plane and metered for the gap
/// beneath it.
///
/// Gap fill is here for exactly that reason and for no other. It is material
/// standing on the layer below just as much as a bead of the wall is, so a
/// column under it did not end and must not be capped as though the part
/// stopped there. What earns it the place is the metering: this list is a
/// promise that whatever covers a raise is measured against it, and gap fill
/// only started keeping that promise when
/// [`brick::extrusion_factor`](crate::brick) began giving a filler the same
/// geometry every other bead gets. Admitted here without that, a gap-fill bead
/// would be metered for a whole layer over a gap half a layer deep — the
/// blob that costs twice what the gap holds, and the one failure this
/// transform must never introduce. The two halves move together or not at all.
fn covers_a_raise(feature: Feature) -> bool {
    matches!(
        feature,
        Feature::ExternalPerimeter | Feature::Overhang | Feature::ThinWall | Feature::GapFill
    )
}

#[derive(Default)]
struct Scan {
    layers: usize,
    declared_height: Option<f64>,
    wall_order: Option<WallOrder>,
    /// Widths exactly as the settings block stated them, since a percentage
    /// only becomes a length once the nozzle has been read, and the nozzle can
    /// be stated after them.
    skin_width: Option<String>,
    wall_width: Option<String>,
    nozzle: Option<String>,
    /// Distinct upward Z steps and how often each was seen, so the commonest
    /// one can stand in for a layer height the file never states.
    z_steps: Vec<(i64, usize)>,
    z_feedrate: Option<f64>,
    bricked: bool,
    contoured: bool,
    arc_extrusions: usize,
    perimeters: usize,
    unknown_regions: usize,
    unknown_region: Option<String>,
    feature: Feature,
    current_z: f64,
    /// Lowest Z of the layer being read, and of the one before it. A Z-hop
    /// only ever raises the nozzle, so the lowest Z of a layer is the layer's
    /// own height and comparing those is what tells a return to the bed apart
    /// from a hop.
    layer_floor: Option<f64>,
    previous_floor: Option<f64>,
    /// Index of the layer whose Z is being collected. `None` before the first
    /// layer of the file, since a start G-code that lifts the nozzle to prime
    /// is not a layer.
    open_layer: Option<usize>,
    /// True once the file has shown a layer-change marker of its own, which is
    /// what says the boundaries below it are not needed.
    saw_markers: bool,
    /// Layer boundaries for a file that states none, so its walls are still
    /// weighed against the layers either side of them.
    markerless: Markerless,
    /// Height measured for each layer so far, indexed by layer.
    layer_heights: Vec<f64>,
    /// Layers whose recorded height is a plane above the bed rather than a
    /// rise from the layer below: the first layer that commanded a height,
    /// and the first of every object after it. It is not layer zero by
    /// definition — a marker the start G-code emits before the first `G1 Z`
    /// takes that number without ever standing anywhere.
    planes: Vec<usize>,
    /// Layers at which the print went back down to start another object.
    object_starts: Vec<usize>,
    /// Last layer seen to extrude an internal perimeter, and what that stood
    /// at when the open layer began. An object start is only recognised once
    /// the layer that returned to the bed has been read, so the snapshot is
    /// what the object before it topped out at.
    last_wall_layer: Option<usize>,
    wall_top_at_open: Option<usize>,
    object_tops: Vec<usize>,
    /// Where the nozzle stands, so an extrusion can be traced from where it
    /// began rather than from where it ended.
    at: (f64, f64),
    /// The positioning mode and units every coordinate above is read in, so a
    /// `G91` section's displacements and a `G20` section's inches are measured
    /// as the millimetre places they reach.
    modal: Modal,
    extruder: Extruder,
    /// Cells the open layer's internal perimeters run through, and the same
    /// for the layer below it. Only two layers are ever held: the answer for a
    /// layer is settled as soon as the one above it has been read.
    here: Cells,
    below: Cells,
    /// Cells of the open layer that are not a hidden wall but still stand over
    /// one \u2014 see [`covers_a_raise`]. Kept apart from `here` because the two
    /// answer different questions: this one only ever says what covers the
    /// layer below, where `here` also says which columns begin on this layer
    /// and so have nothing to climb from.
    covering: Cells,
    /// Index of the layer `below` describes, which is not `open_layer - 1`
    /// when a layer holds no wall at all.
    below_layer: Option<usize>,
    uncovered: Vec<Cells>,
    unsupported: Vec<Cells>,
    /// True once a move that could not be followed has been reported.
    warned: bool,
    /// The box the part's own extrusions cover, in mm.
    footprint: Option<[f64; 4]>,
}

impl Scan {
    /// Books a region marker whose label named nothing, keeping the first of
    /// them to quote.
    ///
    /// The label borrows the line it arrived on, and the line is gone by the
    /// time anyone reports it, so the sample is copied here or not at all —
    /// once per file, whatever the file goes on to say.
    fn note_unrecognised(&mut self, marker: &str) {
        let Some(label) = unrecognised_region(marker) else {
            return;
        };
        self.unknown_regions += 1;
        if self.unknown_region.is_none() {
            self.unknown_region = Some(label.to_owned());
        }
    }

    fn feed(&mut self, raw: &str) {
        // The plane is read now: a wall has to be traced to work out what, if
        // anything, stands on it a layer later.
        let line = Line::parse(raw);

        if let Some(comment) = line.comment() {
            // A stamp rides the Z move it was written beside, so this cannot
            // be folded into the marker handling below.
            let comment = comment.trim_start();
            self.bricked |=
                comment.starts_with(BRICK_STAMP) || comment.starts_with(LEGACY_BRICK_STAMP);
            self.contoured |=
                comment.starts_with(ZAA_STAMP) || comment.starts_with(LEGACY_ZAA_STAMP);
        }

        if let Some(marker) = line.marker() {
            if is_layer_marker(marker) {
                // A start G-code draws its purge line before the first marker,
                // and the rule below has no way to know a marker is coming, so
                // that bead opens a layer of its own. Everything it laid out
                // is dropped here rather than counted twice.
                if !self.saw_markers {
                    self.saw_markers = true;
                    self.forget_the_markerless_layout();
                }
                self.layers += 1;
                self.feature = Feature::Other;
                self.close_layer();
                self.open_layer = Some(self.layers - 1);
                self.wall_top_at_open = self.last_wall_layer;
            } else if let Some(feature) = Feature::from_marker(marker) {
                self.feature = feature;
                if feature.is_perimeter() {
                    self.perimeters += 1;
                }
                // Only a label that classified as nothing can be one this
                // tool has never met, so the cheap test runs first and the
                // marker is taken apart a second time for no other file.
                if feature == Feature::Other {
                    self.note_unrecognised(marker);
                }
            } else if let Some((key, value)) = setting(marker) {
                if key.eq_ignore_ascii_case("layer_height") {
                    if let Ok(height) = value.parse() {
                        self.declared_height.get_or_insert(height);
                    }
                } else if is_skin_width(key) {
                    self.skin_width.get_or_insert_with(|| value.to_owned());
                } else if is_wall_width(key) {
                    self.wall_width.get_or_insert_with(|| value.to_owned());
                } else if key.eq_ignore_ascii_case("nozzle_diameter") {
                    self.nozzle.get_or_insert_with(|| value.to_owned());
                } else if key.eq_ignore_ascii_case("wall_sequence")
                    || key.eq_ignore_ascii_case("external_perimeters_first")
                {
                    self.wall_order.get_or_insert(slicer::wall_order(value));
                }
            }
            return;
        }

        // A number is not a place until the mode it is read in is known: under
        // `G91` it is a displacement and under `G20` it is an inch.
        let moved = self.modal.apply(&line);

        match line.code {
            Code::AbsoluteE | Code::RelativeE => self.extruder.set_mode(line.code),
            // A `G92` moves the origin rather than the filament, so it is not
            // an extrusion and must not be booked as one. It does move the
            // frame every later coordinate is named in, so the next move starts
            // from where the reset says the toolhead stands — traced from where
            // it stood before, a streak of cells is drawn clear across the part
            // that nothing ever printed.
            Code::SetPosition => {
                if let Some(e) = line.e {
                    self.extruder.set_position(e);
                }
                let (x, y, z) = self.modal.position();
                self.at = (x, y);
                if line.z.is_some() {
                    self.observe_height(z);
                }
            }
            _ => {}
        }
        if !line.draws() {
            return;
        }

        // A slicer names only the axes that change, so where a move starts is
        // wherever the last one left off.
        let from = self.at;
        let to = moved.map_or(from, |(x, y, _)| (x, y));
        self.at = to;
        let delta = line.e.map_or(0.0, |e| self.extruder.observe(e));
        // The same test the rewrite uses to open a loop, so every cell it will
        // ask about is one this pass has already drawn.
        let extrudes = delta > 0.0 && line.draws_in_plane();
        // An arc states a centre relative to where it began, or a radius and
        // nothing else, so where it began is what turns either into a curve.
        let arc = line.arc_between(from, to);

        // A file that states no layers of its own is laid out from its beads.
        // Ahead of the footprint, so this bead is drawn into the layer it
        // opens rather than into the one it just ended.
        if extrudes && !self.saw_markers {
            let plane = self.modal.position().2;
            if self.markerless.opens_a_layer(plane) {
                self.layers += 1;
                self.close_markerless_layer();
                self.markerless.open(plane);
                self.open_layer = Some(self.layers - 1);
                self.wall_top_at_open = self.last_wall_layer;
            }
        }

        if extrudes && self.feature.builds_the_part() {
            // The path, not its ends: an arc bulges outside the box its two
            // ends describe, and a ring drawn as two `G2`s has ends that
            // share a coordinate.
            let [left, front, right, back] = extent(from, to, arc);
            self.footprint = Some(match self.footprint {
                Some([had_left, had_front, had_right, had_back]) => [
                    had_left.min(left),
                    had_front.min(front),
                    had_right.max(right),
                    had_back.max(back),
                ],
                None => [left, front, right, back],
            });
        }

        // Booked on the measured delta and never on the `E` word: under `M82`
        // the word is a position, so a wipe or a retract inside the wall's own
        // region still reads positive and would book a wall on a layer that
        // laid none — which caps the object a layer above its real top and
        // leaves the top wall itself uncapped.
        //
        // A thin wall is the wall itself, narrowed past what two beads fit in,
        // so an object topped by one has its walls end on that layer rather
        // than on the one below — and the column under it is covered by a bead
        // this transform meters against the raise, not capped as though the
        // part stopped there.
        if extrudes && matches!(self.feature, Feature::InternalPerimeter | Feature::ThinWall) {
            self.last_wall_layer = self.open_layer;
        }
        if self.feature == Feature::InternalPerimeter && extrudes {
            if self.open_layer.is_some() {
                self.here.draw(from, to, arc);
            }
        } else if extrudes && covers_a_raise(self.feature) && self.open_layer.is_some() {
            self.covering.draw(from, to, arc);
        }

        if line.code == Code::Arc {
            if self.feature == Feature::InternalPerimeter && delta > 0.0 {
                self.arc_extrusions += 1;
            }
            return;
        }
        // A move that only changes Z is the slicer driving the axis on its
        // own terms, so its feedrate is one this machine is known to accept.
        //
        // Only once printing has begun, though. Start G-code lowers the bed
        // and probes it at a deliberate crawl, and taking the slowest rate in
        // the file hands that crawl to every raise in the print: measured on
        // every real slice here, a `G1 Z5 F300` bed-clearance move set the
        // rate where the slicer's own layer changes run at F600.
        if self.open_layer.is_some()
            && line.z.is_some()
            && !line.is_xy_move()
            && let Some(rate) = line.f.filter(|rate| *rate > 0.0)
        {
            self.z_feedrate = Some(self.z_feedrate.map_or(rate, |slowest| slowest.min(rate)));
        }
        if line.z.is_some() {
            self.observe_height(self.modal.position().2);
        }
    }

    /// Books a height the nozzle was commanded to, in mm, whichever mode the
    /// line that commanded it was read in.
    fn observe_height(&mut self, z: f64) {
        if z != self.current_z {
            let step = ((z - self.current_z) * 1000.0).round() as i64;
            if step > 10 {
                match self.z_steps.iter_mut().find(|(value, _)| *value == step) {
                    Some((_, count)) => *count += 1,
                    None => self.z_steps.push((step, 1)),
                }
            }
            self.current_z = z;
        }
        self.layer_floor = Some(self.layer_floor.map_or(z, |floor: f64| floor.min(z)));
    }

    /// Settles the layer that has just been read against the one below it. A
    /// cell of the lower layer that the upper one does not hold has nothing
    /// standing on it, so a bead raised there would be buried under a bead
    /// metered for a full layer.
    ///
    /// "The upper one" is not only its hidden walls. Whatever the rewrite
    /// buffers, it meters against what the layer below actually left standing
    /// there, so a raise printed over by one of those regions is already
    /// accounted for and does not have to be given back — see
    /// [`covers_a_raise`].
    fn close_footprint(&mut self) {
        let Some(index) = self.open_layer else {
            return;
        };
        self.here.settle();
        self.covering.settle();
        // A move no printer makes leaves cells undrawn, and an answer read off
        // an incomplete set is not a cautious answer but a wrong one. Both
        // fall back to the conservative reading: nothing is known to stand on
        // the layer below, and nothing is known to stand under this one. That
        // costs the layer its bricking and no more, where the other direction
        // leaves a bead half a layer proud under something metered for a whole
        // one.
        // The layer below counts too: every answer here is a difference of the
        // two, so a hole in either one is a hole in the result.
        let unread = self.here.refused() + self.covering.refused() + self.below.refused() > 0;
        if let Some(below) = self.below_layer {
            let left = match unread {
                true => self.below.clone(),
                false => self.below.without(&self.here).without(&self.covering),
            };
            self.record(below, left);
        }
        // The mirror: a cell this layer holds that the one below does not is a
        // column starting here, whose first bead has no seam under it to sit
        // on. At the first layer `below` is empty, so all of it starts here.
        let fresh = match unread {
            true => self.here.clone(),
            false => self.here.without(&self.below),
        };
        if unread {
            self.warn_about_the_trace();
        }
        Self::keep(&mut self.unsupported, index, fresh);
        // Swapped rather than handed over, so the buffer the layer below used
        // is the one this layer fills.
        std::mem::swap(&mut self.below, &mut self.here);
        self.here.clear();
        self.covering.clear();
        self.below_layer = Some(index);
    }

    /// Says once that a layer's beads could not all be followed. The user's
    /// print is already running, so this is never a failure: the layers it
    /// touches are measured as though nothing stood on them, which leaves
    /// their walls where the slicer put them.
    fn warn_about_the_trace(&mut self) {
        if std::mem::replace(&mut self.warned, true) {
            return;
        }
        eprintln!(
            "corbel: warning: a move could not be followed, so the layers holding it are left unbricked"
        );
    }

    fn record(&mut self, layer: usize, cells: Cells) {
        Self::keep(&mut self.uncovered, layer, cells);
    }

    fn keep(into: &mut Vec<Cells>, layer: usize, cells: Cells) {
        if cells.is_empty() {
            return;
        }
        if into.len() <= layer {
            into.resize_with(layer + 1, Cells::default);
        }
        into[layer] = cells;
    }

    /// Closes the layer a file with no markers has just left, at the plane its
    /// beads actually sat on.
    ///
    /// The floor accumulated since the last boundary cannot answer this: the Z
    /// that reaches a layer is commanded before the bead that confirms it, so
    /// what has been accumulated is the next layer's plane and not this one's.
    fn close_markerless_layer(&mut self) {
        self.layer_floor = self.markerless.plane();
        self.close_layer();
    }

    /// Drops everything the markerless rule laid out, for a file that turns
    /// out to state its layers after all. Only a start G-code's purge line can
    /// reach this, so there is never more than one layer to drop.
    fn forget_the_markerless_layout(&mut self) {
        self.markerless = Markerless::default();
        self.layers = 0;
        self.open_layer = None;
        self.below_layer = None;
        self.here.clear();
        self.below.clear();
        self.covering.clear();
        self.uncovered.clear();
        self.unsupported.clear();
        self.layer_heights.clear();
        self.planes.clear();
        self.object_starts.clear();
        self.object_tops.clear();
        self.last_wall_layer = None;
        self.wall_top_at_open = None;
        self.layer_floor = None;
        self.previous_floor = None;
    }

    /// Finishes the layer just read. A layer lower than the one before it can
    /// only mean the nozzle went back to the bed to start another object.
    fn close_layer(&mut self) {
        self.close_footprint();
        let floor = self.layer_floor.take();
        let Some(index) = self.open_layer else {
            return;
        };
        // A layer that commanded no height of its own never moved the plane,
        // so it shares the plane below it: no rise to record, and nothing for
        // the layer above to measure across. Written down rather than left to
        // the resize, so the layer a file ends on is described too.
        let Some(floor) = floor else {
            self.record_height(index, 0.0);
            return;
        };
        let dropped = self.previous_floor.is_some_and(|previous| floor < previous);
        // A layer stands on the one below it, so its own height is how far the
        // plane rose. The first layer of an object stands on the bed instead,
        // and a drop is what says another object just started.
        let height = match self.previous_floor {
            Some(previous) if !dropped => floor - previous,
            _ => floor,
        };
        if dropped || self.previous_floor.is_none() {
            self.planes.push(index);
        }
        self.record_height(index, height);
        if dropped {
            self.object_starts.push(index);
            // Walls this object never reached belong to the one before it, and
            // a file with no wall at all keeps the old answer: the layer below.
            self.object_tops
                .push(self.wall_top_at_open.unwrap_or(index.saturating_sub(1)));
        }
        self.previous_floor = Some(floor);
    }

    /// Records what `layer` measured, growing the run of heights to reach it.
    fn record_height(&mut self, layer: usize, height: f64) {
        if self.layer_heights.len() <= layer {
            self.layer_heights.resize(layer + 1, 0.0);
        }
        self.layer_heights[layer] = height;
    }

    /// The height of every layer that stands on another layer.
    ///
    /// A first layer is its own setting and is routinely thicker than the rest,
    /// so it says nothing about the height the file was sliced at — and every
    /// object has one, since one is where the drop that opens an object lands.
    /// What is recorded for those layers is an absolute plane rather than a
    /// rise, so they are skipped rather than believed. Which layers those are
    /// is measured too: a layer marker before the first `G1 Z` takes number
    /// zero without ever standing anywhere, and counting the real first layer
    /// as a stacked one read its own thicker setting as a varied height for
    /// the whole file.
    fn stacked_heights(&self) -> impl Iterator<Item = f64> + '_ {
        self.layer_heights
            .iter()
            .enumerate()
            .filter(|(layer, height)| !self.planes.contains(layer) && is_a_height(height))
            .map(|(_, height)| *height)
    }

    /// The height the file was sliced at, measured rather than declared.
    ///
    /// A layer's floor is the lowest height that layer commanded, so a Z-hop
    /// cannot reach it, and the commonest step from one floor to the next is
    /// the layer height. The commonest upward Z move is NOT: a print retracts
    /// far more often than it changes layer, so where the hop differs from the
    /// layer the hop wins the vote outright. Measured on
    /// `mini_cube_ps2.8.1.bgcode`, whose container states 0.2: the Z-step
    /// histogram of its own decoded G-code reads 0.218, which raised every
    /// column by 0.109 instead of 0.100. The histogram stands in only where
    /// there are no floors to difference, which is a file that lays no bead at
    /// all: a file with no layer markers is laid out by [`Markerless`] and has
    /// floors of its own.
    fn measured_height(&self) -> Option<f64> {
        let mut floors: Vec<(i64, usize)> = Vec::new();
        for height in self.stacked_heights() {
            let step = (height * 1000.0).round() as i64;
            match floors.iter_mut().find(|(value, _)| *value == step) {
                Some((_, count)) => *count += 1,
                None => floors.push((step, 1)),
            }
        }
        commonest(&floors).or_else(|| commonest(&self.z_steps))
    }

    /// The per-layer heights, but only for a file whose slicer varied them.
    fn varying_heights(&mut self) -> Vec<f64> {
        let mut lowest = f64::INFINITY;
        let mut highest = 0.0f64;
        for height in self.stacked_heights() {
            lowest = lowest.min(height);
            highest = highest.max(height);
        }
        if highest - lowest > SAME_HEIGHT {
            std::mem::take(&mut self.layer_heights)
        } else {
            Vec::new()
        }
    }

    fn finish(mut self) -> Survey {
        match self.saw_markers {
            true => self.close_layer(),
            false => self.close_markerless_layer(),
        }
        // Nothing follows the last layer, so all of its wall is uncovered.
        if let Some(below) = self.below_layer {
            let left = self.below.take();
            self.record(below, left);
        }
        let layer_markers = self.saw_markers;
        // A file that never lays a bead has no layer to count off one, so its
        // upward Z steps stand in — there is nothing else left to count.
        let layers = if self.layers > 0 {
            self.layers
        } else {
            self.z_steps.iter().map(|(_, count)| count).sum()
        };

        let measured = self.measured_height();
        let layer_height = self
            .declared_height
            .filter(is_a_height)
            .or(measured.filter(is_a_height));
        let layer_heights = self.varying_heights();
        let nozzle = width(self.nozzle.as_deref(), None);

        Survey {
            layers: layers.max(1),
            layer_markers,
            layer_height: layer_height.unwrap_or(FALLBACK_LAYER_HEIGHT),
            layer_height_detected: layer_height.is_some(),
            layer_heights,
            wall_order: self.wall_order,
            skin_width: width(self.skin_width.as_deref(), nozzle),
            wall_width: width(self.wall_width.as_deref(), nozzle),
            nozzle,
            z_feedrate: self.z_feedrate,
            bricked: self.bricked,
            contoured: self.contoured,
            arc_extrusions: self.arc_extrusions,
            perimeters: self.perimeters,
            unknown_regions: self.unknown_regions,
            unknown_region: self.unknown_region,
            object_starts: {
                // Every file opens an object at its first layer.
                let mut starts = vec![0];
                starts.extend(self.object_starts);
                starts
            },
            object_tops: {
                // The object still open when the file ends tops out at the last
                // wall seen; a file with no wall at all keeps the last layer.
                let mut tops = self.object_tops;
                tops.push(self.last_wall_layer.unwrap_or(layers.max(1) - 1));
                tops
            },
            uncovered: self.uncovered,
            unsupported: self.unsupported,
            footprint: self.footprint,
        }
    }
}

/// Splits `; layer_height = 0.2` from a slicer's settings block into its key
/// and value, given the text after the `;`. Keys are matched whole, so
/// `first_layer_height` is its own setting rather than a `layer_height` line.
fn setting(comment: &str) -> Option<(&str, &str)> {
    let (key, value) = comment.split_once('=')?;
    Some((key.trim(), value.trim()))
}

/// Settings keys naming the width the visible wall is metered at, across the
/// slicers that state one: `external_perimeter_extrusion_width` (PrusaSlicer
/// and SuperSlicer) and `outer_wall_line_width` (OrcaSlicer and Bambu Studio).
fn is_skin_width(key: &str) -> bool {
    [
        "external_perimeter_extrusion_width",
        "outer_wall_line_width",
    ]
    .iter()
    .any(|known| key.eq_ignore_ascii_case(known))
}

/// Settings keys naming the width the hidden walls are metered at:
/// `perimeter_extrusion_width` (PrusaSlicer and SuperSlicer) and
/// `inner_wall_line_width` (OrcaSlicer and Bambu Studio). Matched whole, so
/// the external wall's own key is not one of them.
fn is_wall_width(key: &str) -> bool {
    ["perimeter_extrusion_width", "inner_wall_line_width"]
        .iter()
        .any(|known| key.eq_ignore_ascii_case(known))
}

/// A width from a settings block, in mm.
///
/// Slicers state one either as a length or as a percentage of the nozzle, and
/// a profile covering several extruders states one value per extruder. A
/// percentage without a nozzle to measure it against is no width at all.
pub(crate) fn width(stated: Option<&str>, nozzle: Option<f64>) -> Option<f64> {
    let first = stated?.split(',').next()?.trim();
    let width = match first.strip_suffix('%') {
        Some(share) => share.trim().parse::<f64>().ok()? / 100.0 * nozzle?,
        None => first.parse().ok()?,
    };
    Some(width).filter(is_a_height)
}

/// The most a bead can be tall or wide, in mm.
///
/// The widest nozzle anyone sells is about 1.4 mm and nothing lays a bead
/// thicker than the nozzle it comes out of, so ten is seven times any real
/// profile. What it is for is the other end: a settings line that parses as a
/// number without being one, which without a ceiling reaches the surface
/// transform as a rise and comes out as a commanded height of `-3.1e11`.
const MAX_BEAD: f64 = 10.0;

/// Rejects the values a broken settings line can still parse as a number.
pub(crate) fn is_a_height(height: &f64) -> bool {
    height.is_finite() && *height > 0.0 && *height <= MAX_BEAD
}

/// The commonest of a tally of micron counts, in mm.
///
/// A tie goes to the smaller value, so the answer cannot depend on the order
/// the file happened to present them in.
fn commonest(counts: &[(i64, usize)]) -> Option<f64> {
    counts
        .iter()
        .max_by_key(|(step, count)| (*count, std::cmp::Reverse(*step)))
        .map(|(step, _)| *step as f64 / 1000.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The surface transform spends a fixed number of cells on whatever the
    /// print covers, so the box it is given has to be the part's and not the
    /// bed's. A skirt is drawn a long way outside the part and would spend
    /// most of that budget on empty air.
    #[test]
    fn the_box_is_measured_over_what_builds_the_part_alone() {
        let gcode = "\
M83
;TYPE:Skirt
G1 X-40 Y-40 E1
G1 X40 Y40 E1
;TYPE:External perimeter
G1 X-5 Y-2
G1 X7 Y3 E1
G1 X-5 Y-2 E1
;TYPE:Custom
G1 X99 Y99 E1
";
        let [left, front, right, back] = Survey::of(gcode).footprint.expect("a box");
        assert_eq!((left, front), (-5.0, -2.0));
        assert_eq!((right, back), (7.0, 3.0));

        // A travel names no material, so it says nothing about where the part
        // is either.
        let travelled =
            Survey::of("M83\n;TYPE:External perimeter\nG1 X-5 Y-2\nG1 X0 Y0 E1\nG1 X50 Y50\n");
        assert_eq!(travelled.footprint, Some([-5.0, -2.0, 0.0, 0.0]));
        assert_eq!(
            Survey::of("M83\n;TYPE:Skirt\nG1 X1 Y1 E1\n").footprint,
            None
        );
    }

    /// An arc is not its chord. A slicer with arc fitting on draws a ring as
    /// two 180° `G2`s, whose ends share a coordinate, so a box grown from the
    /// ends alone reports no span at all on that axis — and the grid sized
    /// from it comes out as coarse as it goes, which is under a tenth of the
    /// smoothing the surface transform can do, or does not fit the budget and
    /// is dropped entirely.
    #[test]
    fn the_box_holds_an_arcs_bulge_and_not_just_its_ends() {
        use crate::geometry::Grid;

        // A 20 mm circle centred on (30, 30), drawn as two half turns.
        let gcode = "\
M83
;TYPE:External perimeter
G1 X20 Y30
G2 X40 Y30 I10 J0 E1
G2 X20 Y30 I-10 J0 E1
";
        let survey = Survey::of(gcode);
        let [left, front, right, back] = survey.footprint.expect("a box");
        assert_eq!((left, right), (20.0, 40.0));
        assert_eq!((front, back), (20.0, 40.0));

        // And the grid sized from it is the fine one, not the coarsest there
        // is: a 20 mm part has room for every cell it can use.
        let grid = Grid::for_span(right - left, back - front, crate::zaa::surface::MAX_WINDOW);
        assert_eq!(grid.cell(), Grid::FINEST);
    }

    /// A file in inches states every coordinate in them, so a part read as
    /// millimetres comes out a twenty-fifth of its real size — and the grid
    /// the surface transform sizes from that box is then far too coarse for
    /// the part it is really covering.
    #[test]
    fn a_part_stated_in_inches_is_measured_in_millimetres() {
        let gcode = "\
G20
M83
;TYPE:External perimeter
G1 X0 Y0
G1 X1 Y2 E1
";
        assert_eq!(Survey::of(gcode).footprint, Some([0.0, 0.0, 25.4, 50.8]));
    }

    /// A slicer's custom G-code switches to relative positioning to make a
    /// lift or a nudge it can write without knowing where the toolhead is.
    /// Read as absolute, the displacement is a place, and the bead after it is
    /// traced from the wrong end.
    #[test]
    fn a_move_made_in_relative_positioning_lands_where_it_displaces_to() {
        let gcode = "\
M83
;TYPE:External perimeter
G1 X10 Y10
G91
G1 X5 Y0 E1
G1 X0 Y5 E1
G90
G1 X10 Y10 E1
";
        assert_eq!(Survey::of(gcode).footprint, Some([10.0, 10.0, 15.0, 15.0]));
    }

    /// A `G92 X Y` states where the toolhead already is rather than moving it,
    /// so a survey that ignores it goes on tracing from where it stood — which
    /// draws a streak of cells clear across the part that nothing printed, and
    /// grows the box out to reach it.
    #[test]
    fn a_reset_origin_moves_where_the_next_bead_is_traced_from() {
        let gcode = "\
M83
;TYPE:External perimeter
G1 X50 Y50
G1 X60 Y50 E1
G92 X0 Y0
G1 X2 Y0 E1
";
        assert_eq!(Survey::of(gcode).footprint, Some([0.0, 0.0, 60.0, 50.0]));
    }

    #[test]
    fn prefers_the_declared_layer_height() {
        let survey = Survey::of("; layer_height = 0.25\nG1 Z0.4\nG1 Z0.8\n");
        assert_eq!(survey.layer_height, 0.25);
        assert!(survey.layer_height_detected);
    }

    #[test]
    fn ignores_related_settings_keys() {
        let survey = Survey::of("; first_layer_height = 0.3\n; layer_height = 0.15\n");
        assert_eq!(survey.layer_height, 0.15);
    }

    /// The width the visible wall was metered at is what turns a flow
    /// multiplier into a distance to draw that wall in by.
    #[test]
    fn reads_the_width_the_visible_wall_was_metered_at() {
        for stated in [
            "; external_perimeter_extrusion_width = 0.45",
            "; outer_wall_line_width = 0.45",
        ] {
            assert_eq!(Survey::of(stated).skin_width, Some(0.45), "{stated}");
        }
    }

    /// A percentage is of the nozzle, so it is a width once the file states
    /// one and nothing at all before that.
    #[test]
    fn a_width_stated_as_a_share_of_the_nozzle_needs_the_nozzle() {
        assert_eq!(
            Survey::of("; external_perimeter_extrusion_width = 105%").skin_width,
            None
        );
        let survey = Survey::of(
            "; nozzle_diameter = 0.4\n\
             ; external_perimeter_extrusion_width = 105%\n\
             ; perimeter_extrusion_width = 112.5%\n",
        );
        assert_eq!(survey.nozzle, Some(0.4));
        let close = |got: Option<f64>, want: f64| got.is_some_and(|got| (got - want).abs() < 1e-9);
        assert!(close(survey.skin_width, 0.42), "{:?}", survey.skin_width);
        assert!(close(survey.wall_width, 0.45), "{:?}", survey.wall_width);
    }

    /// The width the hidden walls were metered at is what sets the spacing
    /// their beads were laid at, which is what the flow is derived from.
    #[test]
    fn reads_the_width_the_hidden_walls_were_metered_at() {
        for stated in [
            "; perimeter_extrusion_width = 0.45",
            "; inner_wall_line_width = 0.45",
        ] {
            assert_eq!(Survey::of(stated).wall_width, Some(0.45), "{stated}");
        }
        // The visible wall's own width is a different setting.
        assert_eq!(
            Survey::of("; external_perimeter_extrusion_width = 0.42").wall_width,
            None
        );
    }

    /// A profile covering several extruders states one value per extruder, and
    /// the first is the one this reads.
    #[test]
    fn a_setting_stated_once_per_extruder_reads_as_its_first_value() {
        let survey = Survey::of("; nozzle_diameter = 0.4,0.6\n; inner_wall_line_width = 0.45,0.6");
        assert_eq!(survey.nozzle, Some(0.4));
        assert_eq!(survey.wall_width, Some(0.45));
    }

    /// A width that is not a length is no better than a missing one.
    #[test]
    fn a_wall_width_that_is_not_a_length_is_ignored() {
        for ignored in [
            "; external_perimeter_extrusion_width = 0",
            "; external_perimeter_extrusion_width = -0.45",
            "; external_perimeter_extrusion_width = wide",
            "; nozzle_diameter = 0.4\n; external_perimeter_extrusion_width = -105%",
        ] {
            assert_eq!(Survey::of(ignored).skin_width, None, "{ignored}");
        }
    }

    /// Wall order cannot be measured from the moves — marker transitions come
    /// out 50/50 whichever order was used — but slicers append the setting to
    /// the file, which is the only source a run by hand has.
    #[test]
    fn reads_the_wall_order_the_file_states() {
        let of = |text: &str| Survey::of(text).wall_order;
        assert_eq!(
            of("; wall_sequence = outer wall/inner wall\n"),
            Some(WallOrder::ExternalFirst)
        );
        assert_eq!(
            of("; wall_sequence = inner wall/outer wall\n"),
            Some(WallOrder::InternalFirst)
        );
        assert_eq!(
            of("; wall_sequence = inner-outer-inner wall\n"),
            Some(WallOrder::InternalFirst)
        );
        // PrusaSlicer states it as a flag under its own name.
        assert_eq!(
            of("; external_perimeters_first = 1\n"),
            Some(WallOrder::ExternalFirst)
        );
        assert_eq!(
            of("; external_perimeters_first = 0\n"),
            Some(WallOrder::InternalFirst)
        );
        assert_eq!(of("G1 X1 Y1 E1\n"), None);
    }

    /// A trailing comment on a move is not a settings line, or a wipe command
    /// mentioning a key would redefine the print.
    #[test]
    fn a_setting_is_only_read_from_a_bare_comment() {
        assert_eq!(Survey::of("G1 X1 ; layer_height = 9\n").layer_height, 0.2);
        assert_eq!(
            Survey::of("G1 X1 ; wall_sequence = outer\n").wall_order,
            None
        );
    }

    #[test]
    fn rejects_heights_that_are_not_a_length() {
        let survey = Survey::of("; layer_height = -1\nG1 Z0.3\n");
        assert_eq!(survey.layer_height, 0.3);
    }

    #[test]
    fn measures_the_layer_height_from_z_steps() {
        let survey = Survey::of("G1 Z0.2\nG1 Z0.4\nG1 Z0.6\nG1 Z1.6\n");
        assert_eq!(survey.layer_height, 0.2);
        assert!(survey.layer_height_detected);
    }

    /// A print retracts far more often than it changes layer, so on a file
    /// with hop enabled the commonest upward Z move is the hop and not the
    /// layer. A layer's floor is the lowest height that layer commanded,
    /// which a hop cannot reach, so the step from one floor to the next is
    /// the answer wherever the file has layers to take it from.
    #[test]
    fn a_z_hop_is_not_mistaken_for_the_layer_height() {
        let mut text = String::new();
        for layer in 1..=6usize {
            let plane = 0.2 * layer as f64;
            text.push_str(&format!(";LAYER_CHANGE\nG1 Z{plane:.2}\n"));
            // Two retractions a layer, each lifting and putting back, so the
            // 0.6 mm step outnumbers the 0.2 mm one four to one.
            for _ in 0..2 {
                text.push_str("G1 X1 Y1 E1\n");
                text.push_str(&format!("G1 Z{:.2}\nG1 Z{plane:.2}\n", plane + 0.6));
            }
        }
        let survey = Survey::of(&text);
        assert!(survey.layer_height_detected);
        assert_eq!(survey.layer_height, 0.2);
    }

    /// The survey is the last place a height can arrive, so it is filtered
    /// there too. Without it a bed-clearance move becomes the layer height
    /// and comes back out as a commanded raise no printer could make.
    #[test]
    fn a_measured_step_no_bead_could_be_is_refused() {
        let survey = Survey::of("G1 Z50\nG1 X1 Y1 E1\nG1 Z100\nG1 X1 Y1 E1\nG1 Z150\n");
        assert_eq!(survey.layer_height, FALLBACK_LAYER_HEIGHT);
        assert!(!survey.layer_height_detected);
    }

    /// Half of one height for the whole file staggers every layer that is not
    /// that height by the wrong amount, and an adaptive slice has almost none
    /// that are. On a real Benchy the layers ran 0.081 to 0.119 mm against a
    /// declared 0.2, so the declared figure described no layer in the file.
    #[test]
    fn measures_the_height_of_each_layer_where_the_slicer_varied_it() {
        let survey = Survey::of(
            ";LAYER_CHANGE\nG1 Z0.2\n;LAYER_CHANGE\nG1 Z0.4\n\
             ;LAYER_CHANGE\nG1 Z0.5\n;LAYER_CHANGE\nG1 Z0.8\n",
        );
        assert!(survey.variable_layers());
        for (layer, expected) in [(0, 0.2), (1, 0.2), (2, 0.1), (3, 0.3)] {
            let measured = survey.layer_heights[layer];
            assert!(
                (measured - expected).abs() < 1e-9,
                "layer {layer} measured {measured}, not {expected}"
            );
        }
    }

    /// A file sliced at one height has to reach the transform exactly as it
    /// did before layers were measured at all, so it carries no per-layer
    /// heights for the arithmetic to pick up.
    #[test]
    fn a_file_sliced_at_one_height_measures_no_layers_of_its_own() {
        let survey =
            Survey::of(";LAYER_CHANGE\nG1 Z0.2\n;LAYER_CHANGE\nG1 Z0.4\n;LAYER_CHANGE\nG1 Z0.6\n");
        assert!(!survey.variable_layers());
        assert!(survey.layer_heights.is_empty());
    }

    /// A first layer is its own setting and is routinely thicker than the
    /// rest, so counting it would make every stock profile look adaptive.
    #[test]
    fn a_thicker_first_layer_is_not_a_varied_height() {
        let survey = Survey::of(
            ";LAYER_CHANGE\nG1 Z0.3\n;LAYER_CHANGE\nG1 Z0.5\n\
             ;LAYER_CHANGE\nG1 Z0.7\n;LAYER_CHANGE\nG1 Z0.9\n",
        );
        assert!(!survey.variable_layers(), "{:?}", survey.layer_heights);
    }

    /// A file that completes objects one at a time has a first layer per
    /// object, each measured from the bed rather than from the layer below.
    #[test]
    fn a_second_objects_first_layer_is_not_a_varied_height() {
        let survey = Survey::of(
            ";LAYER_CHANGE\nG1 Z0.3\n;LAYER_CHANGE\nG1 Z0.5\n;LAYER_CHANGE\nG1 Z0.7\n\
             ;LAYER_CHANGE\nG1 Z0.3\n;LAYER_CHANGE\nG1 Z0.5\n;LAYER_CHANGE\nG1 Z0.7\n",
        );
        assert_eq!(survey.objects(), 2);
        assert!(!survey.variable_layers(), "{:?}", survey.layer_heights);
    }

    #[test]
    fn counts_the_wall_extrusions_emitted_as_arcs() {
        // Arc fitting replaces runs of short segments with G2/G3, which no
        // rescaling reaches. The fixture states its extruder mode because an
        // arc is booked on the filament it moves: under `M82` the same `E`
        // word twice is a position the nozzle has already reached.
        let survey = Survey::of(
            "M83\n\
             ;TYPE:Perimeter\n\
             G1 X1 Y1 E0.5\n\
             G3 X2 Y2 I1 J1 E0.5\n\
             G2 X3 Y3 I1 J1 E0.5\n\
             G3 Z2 I1 J1\n\
             ;TYPE:External perimeter\n\
             G3 X4 Y4 I1 J1 E0.5\n",
        );
        assert_eq!(survey.arc_extrusions, 2);
    }

    /// A wipe is a move made with the nozzle already primed, and under `M82` —
    /// PrusaSlicer's default — its `E` word is a position, so it stays
    /// positive while the filament goes nowhere or backwards. Reading the word
    /// instead of the delta booked a wall on a layer that laid none, which
    /// caps the object a layer above its real top and leaves the topmost wall
    /// standing proud under the surface that closes it: the ~2x blob capping
    /// exists to prevent.
    #[test]
    fn a_wipe_over_the_last_wall_is_not_a_wall() {
        let survey = Survey::of(
            "M82\n\
             ;LAYER_CHANGE\nG1 Z0.2\n\
             ;TYPE:Perimeter\nG1 X0 Y0 F9000\nG1 X10 Y0 E1.0\nG1 X10 Y10 E2.0\n\
             ;LAYER_CHANGE\nG1 Z0.4\n\
             ;TYPE:Perimeter\nG1 X0 Y0 F9000\nG1 X10 Y0 E3.0\nG1 X10 Y10 E4.0\n\
             ;LAYER_CHANGE\nG1 Z0.6\n\
             ;TYPE:Perimeter\nG1 X5 Y5 E4.0\nG1 X0 Y5 E3.6\n",
        );
        assert_eq!(survey.layers, 3);
        assert_eq!(survey.object_tops, [1], "the top layer only wiped");
        assert!(survey.closes_an_object(1) && !survey.closes_an_object(2));
    }

    /// The same, for a wall the slicer emitted as arcs. A retract still names
    /// a positive `E`, so it counted as an arc extrusion as well as a wall.
    #[test]
    fn an_arc_that_pulls_filament_back_is_neither_a_wall_nor_an_extrusion() {
        let survey = Survey::of(
            "M82\n\
             ;LAYER_CHANGE\nG1 Z0.2\n\
             ;TYPE:Perimeter\nG1 X0 Y0 F9000\nG2 X10 Y0 I5 J0 E1.0\n\
             ;LAYER_CHANGE\nG1 Z0.4\n\
             ;TYPE:Perimeter\nG1 X0 Y0 F9000\nG2 X10 Y0 I5 J0 E0.6\n",
        );
        assert_eq!(survey.arc_extrusions, 1);
        assert_eq!(survey.object_tops, [0]);
    }

    /// A bead that moves along one axis alone is still a bead. Asking for both
    /// coordinates left a wall run straight along X drawn nowhere, so the
    /// layer above had nothing to be weighed against and nothing was capped.
    #[test]
    fn a_bead_along_one_axis_is_still_a_wall() {
        let survey = Survey::of(
            "M83\n;LAYER_CHANGE\nG1 Z0.2\n\
             ;TYPE:Perimeter\nG1 X0 Y0 F9000\nG1 X10 E0.5\n",
        );
        assert_eq!(survey.object_tops, [0]);
        let cells = survey.uncovered(0).expect("the wall this layer laid");
        assert!(cells.holds(5.0, 0.0));
    }

    /// A layer marker with no `G1 Z` before the next one is a layer that never
    /// moved the plane: a marker a start G-code emits of its own, a layer
    /// holding nothing but custom or timelapse code, or the marker a file ends
    /// on. It has no height, and the layer that does stand somewhere is still
    /// the first — reading that one as a stacked layer made its own thicker
    /// setting look like a slicer varying the height, which throws the file's
    /// declared height away for every layer of the print.
    #[test]
    fn a_layer_that_commands_no_height_leaves_its_neighbours_alone() {
        let survey = Survey::of(
            ";LAYER_CHANGE\n\
             ;LAYER_CHANGE\nG1 Z0.3\n\
             ;LAYER_CHANGE\nG1 Z0.5\n\
             ;LAYER_CHANGE\nG1 Z0.7\n\
             ;LAYER_CHANGE\nG1 Z0.9\n",
        );
        assert!(!survey.variable_layers(), "{:?}", survey.layer_heights);
        assert!((survey.layer_height - 0.2).abs() < 1e-9);
        assert!(survey.layer_height_detected);

        // And the layer after a skipped one rose by its own height, not by the
        // two it would have if the skipped layer had taken the plane with it.
        let varied = Survey::of(
            ";LAYER_CHANGE\nG1 Z0.2\n;LAYER_CHANGE\nG1 Z0.4\n\
             ;LAYER_CHANGE\n; nothing but custom G-code\n\
             ;LAYER_CHANGE\nG1 Z0.5\n;LAYER_CHANGE\nG1 Z0.8\n",
        );
        assert!(varied.variable_layers());
        assert_eq!(varied.layer_heights.len(), 5);
        let close = |got: f64, want: f64| (got - want).abs() < 1e-9;
        assert!(close(varied.layer_heights[2], 0.0), "the plane never moved");
        assert!(close(varied.layer_heights[3], 0.1), "one layer's rise");
    }

    /// Both transforms find their work through the region markers the slicer
    /// wrote, so a file carrying none in either dialect is one the run
    /// rewrites to no effect. Counting them is what lets that be said out
    /// loud.
    #[test]
    fn counts_the_perimeter_regions_it_recognised() {
        let stripped = Survey::of("M83\n;LAYER_CHANGE\nG1 Z0.2\nG1 X10 Y0 E0.5\n");
        assert_eq!(stripped.perimeters, 0);
        for dialect in [
            ";TYPE:Perimeter",
            ";TYPE:External perimeter",
            "; FEATURE: Inner wall",
        ] {
            assert_eq!(Survey::of(dialect).perimeters, 1, "{dialect}");
        }
        assert_eq!(Survey::of(";TYPE:Solid infill").perimeters, 0);
    }

    /// A region marker whose label names nothing is how an unsupported dialect
    /// looks from in here: every marker is found and read, and every one of
    /// them classifies as nothing. Counting them, and keeping one to quote, is
    /// what turns that from silence into something a run can report.
    ///
    /// A label that is understood must never be counted, or the warning cries
    /// wolf on every file: that goes for the regions neither transform owns
    /// (solid infill) and for the ones that are recognised and simply are not
    /// the part (a skirt, a support).
    #[test]
    fn counts_the_region_labels_it_did_not_recognise() {
        let unknown = Survey::of(
            "M83\n;LAYER_CHANGE\nG1 Z0.2\n\
             ;TYPE:Widget\nG1 X10 Y0 E0.5\n\
             ;TYPE:Flange\nG1 X10 Y10 E0.5\n",
        );
        assert_eq!(unknown.unknown_regions, 2);
        assert_eq!(unknown.unknown_region.as_deref(), Some("Widget"));

        for known in [
            ";TYPE:Perimeter",
            ";TYPE:Solid infill",
            ";TYPE:Skirt/Brim",
            "; FEATURE: Support",
            ";TYPE:",
            ";LAYER_CHANGE",
            "; layer_height = 0.2",
        ] {
            let survey = Survey::of(known);
            assert_eq!(survey.unknown_regions, 0, "{known}");
            assert_eq!(survey.unknown_region, None, "{known}");
        }
    }

    #[test]
    fn finds_the_wall_that_nothing_stands_on() {
        // Two walls 20 mm apart, only one of which carries on to the layer
        // above. A raised bead on the other would be buried by whatever the
        // slicer prints over it next.
        let survey = Survey::of(
            "M83\n\
             ;LAYER_CHANGE\nG1 Z0.2 F600\n\
             ;TYPE:Perimeter\n\
             G1 X0 Y0 F9000\nG1 X10 Y0 E0.5\nG1 X10 Y10 E0.5\n\
             G1 X30 Y0 F9000\nG1 X40 Y0 E0.5\nG1 X40 Y10 E0.5\n\
             ;LAYER_CHANGE\nG1 Z0.4 F600\n\
             ;TYPE:Perimeter\n\
             G1 X0 Y0 F9000\nG1 X10 Y0 E0.5\nG1 X10 Y10 E0.5\n",
        );
        let cells = survey
            .uncovered(0)
            .expect("the wall that stops is uncovered");
        assert!(cells.holds(35.0, 0.0), "the wall that stops");
        assert!(!cells.holds(5.0, 0.0), "the wall that carries on");
        // Nothing follows the last layer, so all of it is uncovered.
        let top = survey
            .uncovered(1)
            .expect("nothing stands on the last layer");
        assert!(top.holds(5.0, 0.0));
    }

    /// A move no printer makes cannot be rasterised, and the cells along it
    /// are then never drawn. An outline with a hole in it is not a smaller
    /// outline: read as one it says the layer above covers nothing there, so
    /// the wall below reads as ending and is capped where it carries on, or —
    /// the expensive direction — a wall that really does end reads as covered
    /// and keeps a bead half a layer proud under a surface metered for a whole
    /// one. Both layers fall back to the reading that leaves the wall exactly
    /// where the slicer put it.
    #[test]
    fn a_layer_whose_beads_cannot_all_be_followed_covers_nothing_below_it() {
        let wall = "G1 X0 Y0 F9000\nG1 X10 Y0 E0.5\nG1 X10 Y10 E0.5\n";
        let followed = Survey::of(&format!(
            "M83\n\
             ;LAYER_CHANGE\nG1 Z0.2 F600\n;TYPE:Perimeter\n{wall}\
             ;LAYER_CHANGE\nG1 Z0.4 F600\n;TYPE:Perimeter\n{wall}"
        ));
        assert!(
            followed.uncovered(0).is_none(),
            "the wall carries on, so nothing of it is uncovered"
        );
        // Twenty metres is past what any grid can walk, so the layer's
        // outline comes back with everything but the move's two ends missing.
        let refused = Survey::of(&format!(
            "M83\n\
             ;LAYER_CHANGE\nG1 Z0.2 F600\n;TYPE:Perimeter\n{wall}\
             ;LAYER_CHANGE\nG1 Z0.4 F600\n;TYPE:Perimeter\n{wall}\
             G1 X20000 Y0 E5\n"
        ));
        let cells = refused
            .uncovered(0)
            .expect("a layer that could not be read covers nothing below it");
        assert!(
            cells.holds(5.0, 0.0),
            "the wall below is capped though the layer above stands on it"
        );
        let starts = refused
            .unsupported(1)
            .expect("and nothing under it is known to hold it up");
        assert!(starts.holds(5.0, 0.0));
    }

    /// The same two walls as [`finds_the_wall_that_nothing_stands_on`], in a
    /// file that never says where its layers begin.
    ///
    /// **This replaces a test that pinned the defect.** It asserted that such
    /// a file reports no coverage at all — the answer was "do not know" — and
    /// the cost was that nothing was ever capped and every column read as
    /// fully aged from its first bead. There is no guess in the answer now: a
    /// layer is opened by the first bead laid off the plane the last one sat
    /// on, which is the rule the rewrite follows too, so both passes number
    /// the same layers.
    #[test]
    fn a_file_with_no_layer_markers_is_laid_out_from_its_beads() {
        let survey = Survey::of(
            "M83\n\
             G1 Z0.2 F600\n\
             ;TYPE:Perimeter\n\
             G1 X0 Y0 F9000\nG1 X10 Y0 E0.5\nG1 X10 Y10 E0.5\n\
             G1 X30 Y0 F9000\nG1 X40 Y0 E0.5\nG1 X40 Y10 E0.5\n\
             G1 Z0.4 F600\n\
             G1 X0 Y0 F9000\nG1 X10 Y0 E0.5\nG1 X10 Y10 E0.5\n",
        );
        assert!(
            !survey.layer_markers,
            "the file states no layers of its own"
        );
        assert_eq!(survey.layers, 2);
        let cells = survey
            .uncovered(0)
            .expect("the wall that stops is uncovered");
        assert!(cells.holds(35.0, 0.0), "the wall that stops");
        assert!(!cells.holds(5.0, 0.0), "the wall that carries on");
        let top = survey
            .uncovered(1)
            .expect("nothing stands on the last layer");
        assert!(top.holds(5.0, 0.0));
        // And the mirror: the first layer stands on nothing at all, while the
        // column that carries on into the second is supported all the way.
        let starts = survey
            .unsupported(0)
            .expect("the layer on the bed stands on nothing");
        assert!(starts.holds(5.0, 0.0) && starts.holds(35.0, 0.0));
        assert!(
            survey.unsupported(1).is_none(),
            "a column that carries on is supported"
        );
    }

    /// A hop lifts the nozzle and puts it back before the next bead, so a
    /// layout read off the Z moves counted every hop as a layer. The bead
    /// confirms the layer instead, and the two files below are laid out
    /// identically.
    #[test]
    fn a_hop_opens_no_layer_where_a_file_states_none() {
        let walls = "G1 X0 Y0 F9000\nG1 X10 Y0 E0.5\nG1 X10 Y10 E0.5\n\
                     G1 X30 Y0 F9000\nG1 X40 Y0 E0.5\nG1 X40 Y10 E0.5\n";
        let top = "G1 X0 Y0 F9000\nG1 X10 Y0 E0.5\nG1 X10 Y10 E0.5\n";
        let flat = format!("M83\n;TYPE:Perimeter\nG1 Z0.2 F600\n{walls}G1 Z0.4 F600\n{top}");
        let hopped = format!(
            "M83\n;TYPE:Perimeter\nG1 Z0.2 F600\n\
             G1 Z2.2 F600\nG1 Z0.2 F600\n{walls}\
             G1 Z2.2 F600\nG1 Z0.4 F600\nG1 Z2.4 F600\nG1 Z0.4 F600\n{top}"
        );
        let flat = Survey::of(&flat);
        let hopped = Survey::of(&hopped);
        assert_eq!(flat.layers, 2);
        assert_eq!(hopped.layers, flat.layers, "a hop opened a layer");
        assert_eq!(hopped.object_starts, flat.object_starts);
        for layer in 0..flat.layers {
            for (x, y) in [(5.0, 0.0), (35.0, 0.0)] {
                let holds = |cells: Option<&Cells>| cells.is_some_and(|c| c.holds(x, y));
                assert_eq!(
                    holds(hopped.uncovered(layer)),
                    holds(flat.uncovered(layer)),
                    "layer {layer} is covered differently at {x},{y}"
                );
                assert_eq!(
                    holds(hopped.unsupported(layer)),
                    holds(flat.unsupported(layer)),
                    "layer {layer} is supported differently at {x},{y}"
                );
            }
        }
    }

    #[test]
    fn a_file_without_arcs_reports_none() {
        let survey = Survey::of(";TYPE:Perimeter\nG1 X1 Y1 E0.5\n");
        assert_eq!(survey.arc_extrusions, 0);
    }

    #[test]
    fn falls_back_when_nothing_is_known() {
        let survey = Survey::of("G1 X1 Y1 E1\n");
        assert_eq!(survey.layer_height, FALLBACK_LAYER_HEIGHT);
        assert!(!survey.layer_height_detected);
    }

    #[test]
    fn takes_the_slowest_feedrate_the_file_moves_z_at() {
        // A Z-hop rides the travel rate; a layer change does not. The slower
        // of the two is the one an inserted Z move should borrow.
        let survey = Survey::of(";LAYER_CHANGE\nG1 Z0.2 F600\nG1 Z2.0 F9000\nG1 Z0.4 F720\n");
        assert_eq!(survey.z_feedrate, Some(600.0));
    }

    #[test]
    fn ignores_the_crawl_a_start_gcode_lowers_the_bed_at() {
        // Every real slice measured here opens with a `G1 Z5 F300`
        // bed-clearance move, half the rate the print itself uses. Taking the
        // slowest rate in the whole file hands that crawl to every raise.
        let survey = Survey::of("G1 Z5 F300\n;LAYER_CHANGE\nG1 Z0.2 F600\n");
        assert_eq!(survey.z_feedrate, Some(600.0));
    }

    #[test]
    fn ignores_feedrates_of_moves_that_also_travel() {
        let survey = Survey::of(";LAYER_CHANGE\nG1 X1 Y1 Z0.2 F9000\n");
        assert_eq!(survey.z_feedrate, None);
    }

    #[test]
    fn counts_layers_from_markers() {
        let survey = Survey::of(";LAYER_CHANGE\nG1 Z0.2\n;LAYER_CHANGE\nG1 Z0.4\n");
        assert_eq!(survey.layers, 2);
    }

    /// Printing objects one at a time takes the nozzle back to the bed, which
    /// is the one thing a layer's height cannot otherwise do.
    #[test]
    fn counts_the_objects_a_file_prints_one_after_another() {
        let mut source = String::new();
        for object in 0..3 {
            for layer in 0..4 {
                source.push_str(";LAYER_CHANGE\n");
                source.push_str(&format!("G1 Z{:.3}\n", 0.2 + f64::from(layer) * 0.2));
                source.push_str(&format!("G1 X{object} Y1 E1\n"));
            }
        }
        let survey = Survey::of(&source);
        assert_eq!(survey.objects(), 3);
        assert_eq!(survey.object_starts, [0, 4, 8]);

        // Each object opens on its own first layer and closes on its own last.
        let opens: Vec<usize> = (0..12).filter(|l| survey.opens_an_object(*l)).collect();
        let closes: Vec<usize> = (0..12).filter(|l| survey.closes_an_object(*l)).collect();
        assert_eq!(opens, [0, 4, 8]);
        assert_eq!(closes, [3, 7, 11]);
    }

    #[test]
    fn an_ordinary_print_is_one_object() {
        let source = ";LAYER_CHANGE\nG1 Z0.2\n;LAYER_CHANGE\nG1 Z0.4\n;LAYER_CHANGE\nG1 Z0.6\n";
        let survey = Survey::of(source);
        assert_eq!(survey.objects(), 1);
        assert_eq!(survey.object_starts, [0]);
        assert!(survey.opens_an_object(0) && !survey.opens_an_object(1));
        assert!(survey.closes_an_object(2) && !survey.closes_an_object(1));
        assert_eq!(Survey::of("").objects(), 1);
        assert_eq!(Survey::of("G1 Z0.2\nG1 Z0.4\n").objects(), 1);
    }

    /// A Z-hop only ever raises the nozzle, and the lift a start G-code takes
    /// to prime is not a layer at all. Neither may read as a new object.
    #[test]
    fn hops_and_priming_lifts_are_not_new_objects() {
        let primed = "G1 Z5.0 F600\nG1 X0 Y0 E10\n\
                      ;LAYER_CHANGE\nG1 Z0.2\nG1 X1 Y1 E1\n\
                      ;LAYER_CHANGE\nG1 Z0.4\nG1 X2 Y1 E1\n";
        assert_eq!(
            Survey::of(primed).objects(),
            1,
            "a priming lift is not a layer"
        );

        let hopped = ";LAYER_CHANGE\nG1 Z0.2\nG1 X1 Y1 E1\nG1 Z2.2\nG1 Z0.2\n\
                      ;LAYER_CHANGE\nG1 Z0.4\nG1 X2 Y1 E1\n\
                      ;LAYER_CHANGE\nG1 Z0.6\nG1 Z2.6\nG1 Z0.6\nG1 X3 Y1 E1\n";
        assert_eq!(
            Survey::of(hopped).objects(),
            1,
            "a Z-hop is not a new object"
        );
    }

    /// **This replaces a test that pinned the defect.** It asserted three
    /// layers for the file below, counted off its Z moves — which is what a
    /// hop, a lift and a park all look like, and what made the survey's layer
    /// numbers disagree with the rewrite's. A layer is where a bead was laid,
    /// so a Z move nothing is printed at opens nothing.
    #[test]
    fn counts_layers_from_the_beads_of_a_file_that_states_none() {
        let survey = Survey::of("M83\nG1 Z0.2\nG1 X1 Y1 E1\nG1 Z0.4\nG1 X2 Y1 E1\n");
        assert_eq!(survey.layers, 2);
        assert!(!survey.layer_markers);

        let hopped =
            Survey::of("M83\nG1 Z0.2\nG1 X1 Y1 E1\nG1 Z2.2\nG1 Z0.2\nG1 X2 Y1 E1\nG1 Z0.6\n");
        assert_eq!(
            hopped.layers, 1,
            "a hop, and a lift with nothing printed after it, are not layers"
        );
    }

    #[test]
    fn a_file_always_has_at_least_one_layer() {
        assert_eq!(Survey::of("").layers, 1);
    }

    /// The stamps this tool leaves ride the Z moves it inserts, so they are
    /// trailing comments on a command rather than markers of their own.
    #[test]
    fn recognises_its_own_earlier_work() {
        assert!(Survey::of("G1 Z0.300 F600 ; corbel brick raised\n").bricked);
        assert!(Survey::of("G1 Z0.400 F600 ; corbel brick reset\n").bricked);
        assert!(Survey::of("G1 X1 Y1 Z0.310 E0.1 ; corbel zaa surface\n").contoured);
        assert!(!Survey::of("G1 Z0.4 F600\n; corbel is not a stamp here\n").bricked);
    }

    /// A file processed by a release published under the old name carries the
    /// old spelling and nothing else. It is just as compounded by a second
    /// pass, so it has to be recognised too — and separately per transform,
    /// since a file that was only bricked may still be contoured.
    #[test]
    fn recognises_work_stamped_under_the_old_name() {
        let bricked = Survey::of("G1 Z0.300 F600 ; bricklayers brick raised\n");
        assert!(bricked.bricked);
        assert!(!bricked.contoured);

        let contoured = Survey::of("G1 X1 Y1 Z0.310 E0.1 ; bricklayers zaa surface\n");
        assert!(contoured.contoured);
        assert!(!contoured.bricked);
    }

    /// What decides whether a comment is this tool's own work rather than the
    /// slicer's, so that a second pass does not compound what the first did.
    #[test]
    fn a_stamp_is_recognised_under_either_name() {
        for ours in [
            " corbel brick raised",
            " corbel zaa level",
            " bricklayers brick reset",
            " bricklayers zaa surface",
        ] {
            assert!(is_stamp(ours), "{ours}");
        }
        for theirs in [
            " TYPE:Perimeter",
            " corbelling",
            " printing object Corbel.stl",
            "",
        ] {
            assert!(!is_stamp(theirs), "{theirs}");
        }
    }

    /// A raise rides whatever move the slicer was already making, and that
    /// move may already carry a note of the slicer's own — a wipe, an object
    /// name. The stamp is written in front of it rather than behind, so the
    /// comment on such a line is this tool's text with the slicer's still
    /// following it, and it has to read as this tool's work all the same: a
    /// second pass that missed it would raise a bead already raised.
    #[test]
    fn a_stamp_in_front_of_the_slicers_own_comment_is_still_a_stamp() {
        let ridden = "G1 X1.0 Y1.0 Z0.300 F600 ; corbel brick raised ;WIPE_START";
        let parsed = Line::parse(ridden);
        let comment = parsed.comment().expect("the line carries a comment");
        assert!(is_stamp(comment), "{comment}");
        assert!(Survey::of(&format!("{ridden}\n")).bricked);

        // And it is still not a region marker, or the line the stamp rides
        // would re-declare the region it was inserted into.
        assert!(parsed.marker().is_none(), "{ridden}");

        // The same note with nothing of this tool's in front of it stays the
        // slicer's own.
        let theirs = Line::parse("G1 X1.0 Y1.0 F9000 ; WIPE_START");
        assert!(!is_stamp(theirs.comment().expect("a comment")));
    }

    /// A trailing comment on a move is not a region marker, or a stamped Z
    /// move would re-declare the region it was inserted into.
    #[test]
    fn a_trailing_comment_is_not_a_region_marker() {
        // `M83` for the same reason as `counts_the_wall_extrusions_emitted_as_arcs`:
        // an arc is booked on the filament it moves, and under `M82` the same
        // `E` word twice is a position the nozzle has already reached.
        let survey = Survey::of(
            "M83\n\
             ;TYPE:Perimeter\n\
             G1 X1 Y1 E0.5\n\
             G1 Z0.5 F600 ; TYPE:Solid infill\n\
             G3 X2 Y2 I1 J1 E0.5\n",
        );
        assert_eq!(survey.arc_extrusions, 1, "the region must still be a wall");
    }

    #[test]
    fn z_words_in_comments_do_not_count_as_moves() {
        let survey = Survey::of("G1 X1 Y1 ; move to Z5.0\nG1 X2 Y2 ; and Z9.0\n");
        assert_eq!(survey.layers, 1);
    }

    #[test]
    fn surveying_a_stream_matches_surveying_a_string() {
        let source = "; layer_height = 0.2\n;LAYER_CHANGE\nG1 Z0.2\n;TYPE:Solid infill\n\
                      G1 X1 Y1 E1\n;LAYER_CHANGE\nG1 Z0.4\n;TYPE:Perimeter\nG1 X2 Y2 E1\n";

        for text in [source.to_owned(), source.replace('\n', "\r\n")] {
            let expected = Survey::of(&text);
            let streamed = Survey::read(text.as_bytes()).expect("reading a slice cannot fail");

            assert_eq!(streamed.layers, expected.layers);
            assert_eq!(streamed.layer_markers, expected.layer_markers);
            assert_eq!(streamed.layer_height, expected.layer_height);
            assert_eq!(
                streamed.layer_height_detected,
                expected.layer_height_detected
            );
            assert_eq!(streamed.object_tops, expected.object_tops);
        }
    }
}
