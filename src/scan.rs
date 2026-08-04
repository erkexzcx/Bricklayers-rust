//! A single pre-pass that collects everything the transform needs to know
//! about a file before rewriting it.
//!
//! Nothing here keeps the G-code itself, only a handful of counters, so the
//! pass runs over a stream of any length.

use std::io::{self, BufRead};

use crate::feature::{Feature, is_layer_marker};
use crate::gcode::{Code, Line, Lines};
use crate::slicer::{self, WallOrder};

/// Layer height assumed when the file says nothing useful.
pub const FALLBACK_LAYER_HEIGHT: f64 = 0.2;

/// Feedrate for an inserted Z move when the file never shows one, in mm/min.
/// 12 mm/s is slow enough for any Z axis, including a delta moving all three
/// towers at once.
pub const FALLBACK_Z_FEEDRATE: f64 = 720.0;

/// Comment left on the lines this tool inserts. Repeating the transform
/// compounds it, so a run recognises its own earlier work by these.
pub const BRICK_STAMP: &str = "bricklayers brick ";

#[derive(Clone, Debug)]
pub struct Survey {
    /// Number of printed layers, used to find the first and last one.
    pub layers: usize,
    /// True when the file carries explicit layer-change markers.
    pub layer_markers: bool,
    pub layer_height: f64,
    /// True when the layer height came from the file rather than the fallback.
    pub layer_height_detected: bool,
    /// Height of the first layer, which slicers commonly print thicker than the
    /// rest. `None` when the file never states it; it is never measured, since
    /// a start G-code that lifts the nozzle to prime would be mistaken for it.
    pub first_layer_height: Option<f64>,
    /// Order the file says its walls were printed in, from the configuration
    /// slicers append to the G-code. It cannot be measured from the moves
    /// themselves, so a file processed by hand has no other source.
    pub wall_order: Option<WallOrder>,
    /// Slowest feedrate the file itself uses to move Z alone, in mm/min.
    pub z_feedrate: Option<f64>,
    /// True when [`brick`](crate::brick) has already run over this file.
    pub bricked: bool,
    /// Extrusions inside internal perimeters emitted as `G2`/`G3` arcs, which
    /// pass through untouched however the loop around them is shifted.
    pub arc_extrusions: usize,
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
        while let Some(raw) = lines.next_line()? {
            scan.feed(raw);
        }
        Ok(scan.finish())
    }

    /// Objects the file prints one after another.
    pub fn objects(&self) -> usize {
        self.object_starts.len()
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
}

#[derive(Default)]
struct Scan {
    layers: usize,
    declared_height: Option<f64>,
    declared_first_height: Option<f64>,
    wall_order: Option<WallOrder>,
    /// Distinct upward Z steps and how often each was seen, so the commonest
    /// one can stand in for a layer height the file never states.
    z_steps: Vec<(i64, usize)>,
    z_feedrate: Option<f64>,
    bricked: bool,
    arc_extrusions: usize,
    feature: Feature,
    current_z: f64,
    /// Lowest Z of the layer being read, and of the one before it. A Z-hop
    /// only ever raises the nozzle, so the lowest Z of a layer is the layer's
    /// own height and comparing those is what tells a return to the bed apart
    /// from a hop.
    layer_floor: Option<f64>,
    previous_floor: Option<f64>,
    /// Index of the layer whose Z is being collected. `None` before the first
    /// layer marker, since a start G-code that lifts the nozzle to prime is
    /// not a layer.
    open_layer: Option<usize>,
    /// Layers at which the print went back down to start another object.
    object_starts: Vec<usize>,
    /// Last layer seen to extrude an internal perimeter, and what that stood
    /// at when the open layer began. An object start is only recognised once
    /// the layer that returned to the bed has been read, so the snapshot is
    /// what the object before it topped out at.
    last_wall_layer: Option<usize>,
    wall_top_at_open: Option<usize>,
    object_tops: Vec<usize>,
}

impl Scan {
    fn feed(&mut self, raw: &str) {
        // One parse, and the plane is left unread: the survey only needs to
        // know that a move went somewhere in it, never where.
        let line = Line::scan(raw);

        if let Some(comment) = line.comment() {
            // A stamp rides the Z move it was written beside, so this cannot
            // be folded into the marker handling below.
            let comment = comment.trim_start();
            self.bricked |= comment.starts_with(BRICK_STAMP);
        }

        if let Some(marker) = line.marker() {
            if is_layer_marker(marker) {
                self.layers += 1;
                self.feature = Feature::Other;
                self.close_layer();
                self.open_layer = Some(self.layers - 1);
                self.wall_top_at_open = self.last_wall_layer;
            } else if let Some(feature) = Feature::from_marker(marker) {
                self.feature = feature;
            } else if let Some((key, value)) = setting(marker) {
                if key.eq_ignore_ascii_case("layer_height") {
                    if let Ok(height) = value.parse() {
                        self.declared_height.get_or_insert(height);
                    }
                } else if key.eq_ignore_ascii_case("first_layer_height") {
                    if let Ok(height) = value.parse() {
                        self.declared_first_height.get_or_insert(height);
                    }
                } else if key.eq_ignore_ascii_case("wall_sequence")
                    || key.eq_ignore_ascii_case("external_perimeters_first")
                {
                    self.wall_order.get_or_insert(slicer::wall_order(value));
                }
            }
            return;
        }

        if line.code == Code::Arc {
            if self.feature == Feature::InternalPerimeter && line.e.is_some_and(|e| e > 0.0) {
                self.arc_extrusions += 1;
                self.last_wall_layer = self.open_layer;
            }
            return;
        }
        if line.code != Code::Move {
            return;
        }
        if self.feature == Feature::InternalPerimeter
            && line.is_xy_move()
            && line.e.is_some_and(|e| e > 0.0)
        {
            self.last_wall_layer = self.open_layer;
        }
        // A move that only changes Z is the slicer driving the axis on its
        // own terms, so its feedrate is one this machine is known to accept.
        if line.z.is_some()
            && !line.is_xy_move()
            && let Some(rate) = line.f.filter(|rate| *rate > 0.0)
        {
            self.z_feedrate = Some(self.z_feedrate.map_or(rate, |slowest| slowest.min(rate)));
        }
        if let Some(z) = line.z
            && z != self.current_z
        {
            let step = ((z - self.current_z) * 1000.0).round() as i64;
            if step > 10 {
                match self.z_steps.iter_mut().find(|(value, _)| *value == step) {
                    Some((_, count)) => *count += 1,
                    None => self.z_steps.push((step, 1)),
                }
            }
            self.current_z = z;
        }
        if let Some(z) = line.z {
            self.layer_floor = Some(self.layer_floor.map_or(z, |floor| floor.min(z)));
        }
    }

    /// Finishes the layer just read. A layer lower than the one before it can
    /// only mean the nozzle went back to the bed to start another object.
    fn close_layer(&mut self) {
        let floor = self.layer_floor.take();
        let (Some(index), Some(floor)) = (self.open_layer, floor) else {
            return;
        };
        if self.previous_floor.is_some_and(|previous| floor < previous) {
            self.object_starts.push(index);
            // Walls this object never reached belong to the one before it, and
            // a file with no wall at all keeps the old answer: the layer below.
            self.object_tops
                .push(self.wall_top_at_open.unwrap_or(index.saturating_sub(1)));
        }
        self.previous_floor = Some(floor);
    }

    fn finish(mut self) -> Survey {
        self.close_layer();
        let layer_markers = self.layers > 0;
        let layers = if layer_markers {
            self.layers
        } else {
            self.z_steps.iter().map(|(_, count)| count).sum()
        };

        let measured = self
            .z_steps
            .iter()
            .max_by_key(|(_, count)| *count)
            .map(|(step, _)| *step as f64 / 1000.0);
        let layer_height = self.declared_height.filter(is_a_height).or(measured);

        Survey {
            layers: layers.max(1),
            layer_markers,
            layer_height: layer_height.unwrap_or(FALLBACK_LAYER_HEIGHT),
            layer_height_detected: layer_height.is_some(),
            first_layer_height: self.declared_first_height.filter(is_a_height),
            wall_order: self.wall_order,
            z_feedrate: self.z_feedrate,
            bricked: self.bricked,
            arc_extrusions: self.arc_extrusions,
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

/// Rejects the values a broken settings line can still parse as a number.
pub(crate) fn is_a_height(height: &f64) -> bool {
    height.is_finite() && *height > 0.0
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn reads_the_first_layer_height_as_its_own_setting() {
        let survey = Survey::of("; first_layer_height = 0.3\n; layer_height = 0.15\n");
        assert_eq!(survey.first_layer_height, Some(0.3));
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
    fn the_first_layer_height_is_never_guessed() {
        // A start G-code that lifts the nozzle to prime would otherwise be read
        // as a very thick first layer.
        let survey = Survey::of("G1 Z5 F600\nG1 Z0.2\nG1 Z0.4\n");
        assert_eq!(survey.first_layer_height, None);
    }

    #[test]
    fn rejects_heights_that_are_not_a_length() {
        let survey = Survey::of("; first_layer_height = 0\n; layer_height = -1\nG1 Z0.3\n");
        assert_eq!(survey.first_layer_height, None);
        assert_eq!(survey.layer_height, 0.3);
    }

    #[test]
    fn measures_the_layer_height_from_z_steps() {
        let survey = Survey::of("G1 Z0.2\nG1 Z0.4\nG1 Z0.6\nG1 Z1.6\n");
        assert_eq!(survey.layer_height, 0.2);
        assert!(survey.layer_height_detected);
    }

    #[test]
    fn counts_the_wall_extrusions_emitted_as_arcs() {
        // Arc fitting replaces runs of short segments with G2/G3, which no
        // rescaling reaches.
        let survey = Survey::of(
            ";TYPE:Perimeter\n\
             G1 X1 Y1 E0.5\n\
             G3 X2 Y2 I1 J1 E0.5\n\
             G2 X3 Y3 I1 J1 E0.5\n\
             G3 Z2 I1 J1\n\
             ;TYPE:External perimeter\n\
             G3 X4 Y4 I1 J1 E0.5\n",
        );
        assert_eq!(survey.arc_extrusions, 2);
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
        let survey = Survey::of("G1 Z0.2 F600\nG1 Z2.0 F9000\nG1 Z0.4 F720\n");
        assert_eq!(survey.z_feedrate, Some(600.0));
    }

    #[test]
    fn ignores_feedrates_of_moves_that_also_travel() {
        let survey = Survey::of("G1 X1 Y1 Z0.2 F9000\n");
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

    #[test]
    fn counts_layers_from_z_moves_without_markers() {
        let survey = Survey::of("G1 Z0.2\nG1 X1 Y1 E1\nG1 Z0.4\nG1 Z0.6\n");
        assert_eq!(survey.layers, 3);
        assert!(!survey.layer_markers);
    }

    #[test]
    fn a_file_always_has_at_least_one_layer() {
        assert_eq!(Survey::of("").layers, 1);
    }

    /// The stamps this tool leaves ride the Z moves it inserts, so they are
    /// trailing comments on a command rather than markers of their own.
    #[test]
    fn recognises_its_own_earlier_work() {
        assert!(Survey::of("G1 Z0.300 F600 ; bricklayers brick raised\n").bricked);
        assert!(Survey::of("G1 Z0.400 F600 ; bricklayers brick reset\n").bricked);
        assert!(!Survey::of("G1 Z0.4 F600\n; bricklayers is not a stamp here\n").bricked);
    }

    /// A trailing comment on a move is not a region marker, or a stamped Z
    /// move would re-declare the region it was inserted into.
    #[test]
    fn a_trailing_comment_is_not_a_region_marker() {
        let survey = Survey::of(
            ";TYPE:Perimeter\n\
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
