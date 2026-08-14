//! Reading one layer ahead of the rewrite.
//!
//! [`surface`](crate::zaa::surface) needs the outline of the layer printed **over**
//! the one being written, and a transform that streams has already passed it
//! by the time it gets there. Keeping every layer's outline from the survey
//! would answer that, and would also make the memory this tool uses a function
//! of how tall the print is: a bed-filling layer is a few hundred thousand
//! cells, and there is no bound on how many layers a file has.
//!
//! So the file is read a second time instead, by a reader of its own that
//! never gets more than a layer or two in front. Three layers are held, which
//! is what the surface model compares, and the cost is one more pass over the
//! input rather than one more copy of it.

use std::io::{self, BufRead};

use crate::gcode::feature::{Feature, is_layer_marker};
use crate::gcode::{Code, Extruder, Line, Lines, MAX_LINE, Modal};
use crate::geometry::{Cells, Grid};

/// Layers held at once: the one being written, the one over it and the one
/// under it.
const KEPT: usize = 3;

/// A second pass over the same file, kept a layer ahead of the first.
pub struct Scout<R> {
    lines: Lines<R>,
    trace: Trace,
    /// True while a line too long to be held whole is arriving in pieces. A
    /// piece is not a line: it carries no move and no marker, and a fragment
    /// of one read as either would trace an outline the file never drew.
    spilling: bool,
}

impl<R: BufRead> Scout<R> {
    /// Reads `reader` again, quantising what it finds to `grid`.
    ///
    /// The grid is the surface model's own rather than the one the
    /// wall-stacking test shares: what it has to resolve is the strip a layer
    /// leaves exposed, which is a fraction of a millimetre on any slope steep
    /// enough to be covered by its own wall.
    pub fn new(reader: R, grid: Grid) -> Self {
        Self {
            lines: Lines::new(reader),
            trace: Trace {
                filling: Cells::on(grid),
                ..Trace::default()
            },
            spilling: false,
        }
    }

    /// The footprints of `layer - 1`, `layer` and `layer + 1`, reading as far
    /// ahead as it takes to settle the last of them.
    ///
    /// Layers have to be asked for in order; one already passed reads as
    /// absent, which leaves that layer's surface flat rather than wrong.
    ///
    /// A move that could not be followed is not dropped quietly: the count of
    /// them travels with the layer's own cells, through [`Cells::take`] and
    /// out of here, so the caller can tell an outline with a hole in it from a
    /// smaller outline. It has to, because a strip is the distance from one
    /// outline to the next.
    pub fn around(&mut self, layer: usize) -> io::Result<[Option<&Cells>; 3]> {
        while !self.trace.done && self.trace.newest().is_none_or(|last| last <= layer) {
            match self.lines.next_line()? {
                Some(raw) if !self.spilling && raw.text.len() < MAX_LINE => {
                    self.trace.feed(raw.text)
                }
                Some(raw) => {
                    // Asking whether this is one piece of a longer line costs
                    // a copy, since the text borrows the reader — so it is
                    // only asked of a line near the cap, or of the first line
                    // after one. The last piece answers yes as well, having
                    // been assembled out of what the read before it carried
                    // over, so anything answering no is a line of its own.
                    let text = raw.text.to_owned();
                    self.spilling = self.lines.partial();
                    if !self.spilling {
                        self.trace.feed(&text);
                    }
                }
                None => {
                    self.trace.done = true;
                    self.trace.close();
                }
            }
        }

        let mut found: [Option<&Cells>; 3] = [None, None, None];
        for (index, cells) in &self.trace.kept {
            if *index + 1 == layer {
                found[0] = Some(cells);
            } else if *index == layer {
                found[1] = Some(cells);
            } else if *index == layer + 1 {
                found[2] = Some(cells);
            }
        }
        Ok(found)
    }
}

/// Everything the walk keeps, held apart from the reader so that a line
/// borrowed from one can be handed to the other.
#[derive(Default)]
struct Trace {
    at: (f64, f64),
    /// The positioning mode and units every coordinate is read in. The rewrite
    /// keeps one of its own over the same lines, so both reach the same place
    /// for every move and the same outline for every layer.
    modal: Modal,
    extruder: Extruder,
    feature: Feature,
    /// Index of the layer whose cells are being collected. `None` before the
    /// first layer marker, since a start G-code that primes and lifts is not a
    /// layer.
    open: Option<usize>,
    filling: Cells,
    layers: usize,
    started: bool,
    /// The last few completed layers, oldest first.
    kept: Vec<(usize, Cells)>,
    done: bool,
}

impl Trace {
    fn newest(&self) -> Option<usize> {
        self.kept.last().map(|(index, _)| *index)
    }

    fn feed(&mut self, raw: &str) {
        let line = Line::parse(raw);
        if let Some(text) = line.marker() {
            if is_layer_marker(text) {
                self.close();
                self.feature = Feature::Other;
                // The same count the rewrite keeps: the first marker opens
                // layer zero, so both passes name the same layer.
                self.layers += usize::from(std::mem::replace(&mut self.started, true));
                self.open = Some(self.layers);
            } else if let Some(feature) = Feature::from_marker(text) {
                self.feature = feature;
            }
            return;
        }

        // A number is not a place until the mode it is read in is known: under
        // `G91` it is a displacement and under `G20` it is an inch.
        let moved = self.modal.apply(&line);

        match line.code {
            Code::AbsoluteE | Code::RelativeE => self.extruder.set_mode(line.code),
            // A `G92` moves the origin rather than the filament — but where it
            // names an axis it does move the frame the next coordinate is read
            // in, so the next move starts from where the reset says the
            // toolhead stands.
            Code::SetPosition => {
                if let Some(e) = line.e {
                    self.extruder.set_position(e);
                }
                let (x, y, _) = self.modal.position();
                self.at = (x, y);
            }
            _ => {}
        }
        if !line.draws() {
            return;
        }

        // A slicer names only the axes that change, so a move starts wherever
        // the last one left off.
        let from = self.at;
        let to = moved.map_or(from, |(x, y, _)| (x, y));
        self.at = to;
        let delta = line.e.map_or(0.0, |e| self.extruder.observe(e));
        if delta > 0.0 && self.feature.builds_the_part() && self.open.is_some() {
            // An arc states a centre relative to where it began, or a radius
            // and nothing else, so where it began is what turns either into a
            // curve. A chord in place of the curve puts the outline inside the
            // part, and the strip either side of it is measured from that
            // outline.
            self.filling.draw(from, to, line.arc_between(from, to));
        }
    }

    /// Files the layer just read and starts a fresh one.
    fn close(&mut self) {
        let Some(index) = self.open.take() else {
            self.filling.clear();
            return;
        };
        self.filling.settle();
        if self.kept.len() < KEPT {
            self.kept.push((index, self.filling.take()));
            return;
        }
        // The oldest slot's storage becomes the next layer's, so a file of any
        // height allocates for three layers and no more.
        let (_, mut oldest) = self.kept.remove(0);
        oldest.clear();
        let full = std::mem::replace(&mut self.filling, oldest);
        self.kept.push((index, full));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One layer of a square ring, tagged so it counts toward the object.
    fn layer(size: f64) -> String {
        let half = size / 2.0;
        let corners = [
            (-half, -half),
            (half, -half),
            (half, half),
            (-half, half),
            (-half, -half),
        ];
        let mut text = String::from(";LAYER_CHANGE\n;TYPE:Perimeter\n");
        text.push_str(&format!("G1 X{:.3} Y{:.3}\n", corners[0].0, corners[0].1));
        for (x, y) in &corners[1..] {
            text.push_str(&format!("G1 X{x:.3} Y{y:.3} E0.5\n"));
        }
        text
    }

    /// Slicers declare relative extrusion in their start G-code, and every
    /// fixture here writes `E` words as deltas.
    fn scout_of(source: &str) -> Scout<io::Cursor<Vec<u8>>> {
        Scout::new(
            io::Cursor::new(format!("M83\n{source}").into_bytes()),
            Grid::default(),
        )
    }

    #[test]
    fn a_layer_is_surrounded_by_the_ones_either_side_of_it() {
        let source = format!("{}{}{}", layer(20.0), layer(16.0), layer(12.0));
        let mut scout = scout_of(&source);

        let [below, here, above] = scout.around(1).expect("read");
        assert!(below.is_some_and(|cells| cells.holds(10.0, 0.0)));
        assert!(here.is_some_and(|cells| cells.holds(8.0, 0.0)));
        assert!(above.is_some_and(|cells| cells.holds(6.0, 0.0)));
    }

    /// The first layer has nothing under it and the last has nothing over it,
    /// which is what leaves both of them flat rather than measured against a
    /// layer that is not there.
    #[test]
    fn the_ends_of_a_file_have_a_side_missing() {
        let source = format!("{}{}", layer(20.0), layer(16.0));
        let mut scout = scout_of(&source);
        assert!(scout.around(0).expect("read")[0].is_none(), "nothing below");

        let mut scout = scout_of(&source);
        assert!(scout.around(1).expect("read")[2].is_none(), "nothing above");
    }

    /// A skirt, a brim or a prime tower sits beside the part, and counting one
    /// would put the object's outline wherever the skirt ran.
    #[test]
    fn what_is_not_the_object_is_not_part_of_its_outline() {
        let source = concat!(
            ";LAYER_CHANGE\n",
            ";TYPE:Skirt/Brim\n",
            "G1 X-30 Y-30\n",
            "G1 X30 Y-30 E1\n",
            ";TYPE:Perimeter\n",
            "G1 X-5 Y0\n",
            "G1 X5 Y0 E1\n",
            ";LAYER_CHANGE\n",
        );
        let mut scout = scout_of(source);
        let [_, here, _] = scout.around(0).expect("read");
        let here = here.expect("a layer");
        assert!(here.holds(0.0, 0.0), "the object is traced");
        assert!(!here.holds(0.0, -30.0), "the skirt is not");
    }

    /// A travel lays nothing down, so it must not draw the object's outline
    /// across the middle of a part.
    #[test]
    fn a_travel_leaves_no_footprint() {
        let source = concat!(
            ";LAYER_CHANGE\n",
            ";TYPE:Perimeter\n",
            "G1 X-10 Y0\n",
            "G1 X-9 Y0 E1\n",
            "G1 X9 Y0\n",
            "G1 X10 Y0 E1\n",
            ";LAYER_CHANGE\n",
        );
        let mut scout = scout_of(source);
        let here = scout.around(0).expect("read")[1].expect("a layer");
        assert!(here.holds(-9.5, 0.0));
        assert!(here.holds(9.5, 0.0));
        assert!(!here.holds(0.0, 0.0), "the travel between them");
    }

    /// The rewrite keeps a tracker of its own over these same lines, so a
    /// `G92 X Y` has to move both or the two disagree about where the layer
    /// is: the scout would trace a streak from where the toolhead stood to
    /// where the reset put it, and report the layer above as covering
    /// material nothing ever printed.
    #[test]
    fn a_reset_origin_moves_where_the_next_bead_is_traced_from() {
        let source = concat!(
            ";LAYER_CHANGE\n",
            ";TYPE:Perimeter\n",
            "G1 X40 Y40\n",
            "G92 X0 Y0\n",
            "G1 X10 Y0 E1\n",
            ";LAYER_CHANGE\n",
        );
        let mut scout = scout_of(source);
        let here = scout.around(0).expect("read")[1].expect("a layer");
        assert!(
            here.holds(5.0, 0.0),
            "the bead the reset placed at the origin"
        );
        assert!(
            !here.holds(25.0, 20.0),
            "not a streak back from where it stood"
        );
    }

    /// A displacement is not a place. Read as one, a nudge inside a custom
    /// G-code block drags the outline back towards the origin.
    #[test]
    fn a_relative_move_lands_where_it_displaces_to() {
        let source = concat!(
            ";LAYER_CHANGE\n",
            ";TYPE:Perimeter\n",
            "G1 X20 Y20\n",
            "G91\n",
            "G1 X10 Y0 E1\n",
            "G90\n",
            ";LAYER_CHANGE\n",
        );
        let mut scout = scout_of(source);
        let here = scout.around(0).expect("read")[1].expect("a layer");
        assert!(
            here.holds(25.0, 20.0),
            "ten millimetres on from where it was"
        );
        assert!(!here.holds(15.0, 10.0), "not the line back to X10 Y0");
    }

    /// Everything before the first layer marker is start G-code: a prime line
    /// runs the width of the bed and belongs to no layer at all.
    #[test]
    fn a_prime_line_before_the_first_layer_belongs_to_no_layer() {
        let source = format!(
            "{}{}",
            ";TYPE:Perimeter\nG1 X-100 Y-100\nG1 X100 Y-100 E9\n",
            layer(20.0)
        );
        let mut scout = scout_of(&source);
        let here = scout.around(0).expect("read")[1].expect("a layer");
        assert!(!here.holds(0.0, -100.0));
        assert!(here.holds(10.0, 0.0));
    }

    /// An arc is followed round rather than cut across, so a ring drawn as one
    /// `G3` covers the ring and not its chord.
    #[test]
    fn an_arc_draws_the_curve_it_commands() {
        let source = concat!(
            ";LAYER_CHANGE\n",
            ";TYPE:Perimeter\n",
            "G1 X0 Y0\n",
            "G3 X0 Y0 I5 J0 E9\n",
            ";LAYER_CHANGE\n",
        );
        let mut scout = scout_of(source);
        let here = scout.around(0).expect("read")[1].expect("a layer");
        assert!(here.holds(10.0, 0.0), "the far side of the ring");
        assert!(here.holds(5.0, 5.0), "the top of it");
        assert!(!here.holds(5.0, 0.0), "not the middle");
    }

    /// The other form the same curve is written in. A slicer with arc fitting
    /// on emits either, and a reader that only knew the centre form cut the
    /// radius form across its own chord — which puts the object's outline
    /// through the middle of the part, so the layer above reads as covering
    /// material that is really exposed.
    #[test]
    fn an_arc_written_as_a_radius_draws_the_same_curve() {
        // Half a turn of radius 10 about the origin, from (10, 0) round the
        // top to (-10, 0). Its chord is the X axis and its curve stands
        // 10 mm clear of it.
        let source = concat!(
            ";LAYER_CHANGE\n",
            ";TYPE:Perimeter\n",
            "G1 X10 Y0\n",
            "G3 X-10 Y0 R10 E9\n",
            ";LAYER_CHANGE\n",
        );
        let mut scout = scout_of(source);
        let here = scout.around(0).expect("read")[1].expect("a layer");
        assert!(here.holds(0.0, 10.0), "the top of the arc");
        assert!(!here.holds(0.0, 0.0), "and not the chord across it");
    }

    /// A move no printer makes cannot be rasterised, and the cells along it
    /// are then never drawn. How many were refused travels with the layer's
    /// own cells and out of here, so a caller can tell an outline with a hole
    /// in it from a smaller outline.
    #[test]
    fn a_layer_that_could_not_be_read_says_so() {
        let whole = format!("{}{}", layer(20.0), layer(16.0));
        let mut scout = scout_of(&whole);
        let here = scout.around(0).expect("read")[1].expect("a layer");
        assert_eq!(here.refused(), 0, "every bead of it was followed");

        // Twenty metres is past what any grid can walk.
        let torn = format!("{}G1 X20000 Y0 E5\n{}", layer(20.0), layer(16.0));
        let mut scout = scout_of(&torn);
        let here = scout.around(0).expect("read")[1].expect("a layer");
        assert!(here.refused() > 0, "the move that could not be followed");
    }

    /// Three layers at a time, however tall the file is.
    #[test]
    fn only_three_layers_are_ever_held() {
        let mut source = String::new();
        for step in 0..40 {
            source.push_str(&layer(40.0 - step as f64));
        }
        let mut scout = scout_of(&source);
        for index in 0..38 {
            let (had_below, had_here, had_above) = {
                let [below, here, above] = scout.around(index).expect("read");
                (below.is_some(), here.is_some(), above.is_some())
            };
            assert!(had_here, "layer {index}");
            assert!(had_above, "the layer above {index}");
            assert_eq!(had_below, index > 0, "the layer below {index}");
            assert!(scout.trace.kept.len() <= KEPT);
        }
    }
}
