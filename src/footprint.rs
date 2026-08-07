//! Where a layer's walls sit, quantised to a grid.
//!
//! A raised bead stands half a layer proud of its own plane, so whatever the
//! slicer prints over it at the next plane was metered for a gap twice as tall
//! as the one that is really there. That is only safe where the thing above is
//! another raised bead of the same column. Answering "is there a wall above
//! this one?" needs the next layer's geometry, which the survey has and the
//! rewrite does not, so the survey works it out and hands over the answer.
//!
//! The answer is a set of grid cells rather than a set of paths: a cell is the
//! unit of "these two beads overlap", the comparison is a binary search, and
//! two runs over the same path always produce the same cells however the loop
//! was sampled.

use std::f64::consts::TAU;

/// Grid cell, in mm.
///
/// This is the tolerance of the whole test: two beads count as stacked when
/// their paths share a cell. A bead is around 0.45 mm wide, so at 0.3 mm two
/// beads that share a cell overlap by more than half their width, and two that
/// do not are more than a bead apart. Measured over three real slices, 96.3 to
/// 96.7% of wall path has a wall running within 0.2 mm of it on the layer
/// above and the rest is spread from 0.4 mm out to 3 mm, so anything in that
/// window separates a column that continues from one that ends.
pub const CELL: f64 = 0.3;

/// Sampling step along an arc. Half a cell, so no cell the curve crosses can be
/// stepped over, which is what makes the cells a property of the path rather
/// than of where the sampling happened to start.
const STEP: f64 = CELL / 2.0;

/// Cells one move may be cut into. A move longer than a bed is not a move, and
/// a corrupt coordinate must not turn into an allocation.
const MAX_CELLS: usize = 8192;

/// Grid coordinates per mm.
const PER_MM: f64 = 1.0 / CELL;

/// The centre and direction of a `G2`/`G3`, taken from its `I`/`J` offsets.
#[derive(Clone, Copy, Debug)]
pub struct Arc {
    pub i: f64,
    pub j: f64,
    pub clockwise: bool,
}

/// Calls `visit` once for each grid cell the path of a move passes through, in
/// order and without repeating the one before.
///
/// Arcs are followed round rather than cut across. A slicer with arc fitting on
/// draws a whole ring as two or three `G2`s, and taking their chords would say
/// the ring covers nothing at all.
pub fn cells(from: (f64, f64), to: (f64, f64), arc: Option<Arc>, mut visit: impl FnMut(u32)) {
    let mut last = None;
    let mut emit = |column: i32, row: i32| {
        let cell = key(column, row);
        if last != Some(cell) {
            last = Some(cell);
            visit(cell);
        }
    };

    if let Some(arc) = arc
        && let Some((centre, radius, start, sweep)) = turn(from, to, arc)
    {
        let steps = samples(radius * sweep.abs());
        emit(floor(from.0 * PER_MM), floor(from.1 * PER_MM));
        for step in 1..steps {
            let angle = start + sweep * step as f64 / steps as f64;
            emit(
                floor((centre.0 + radius * angle.cos()) * PER_MM),
                floor((centre.1 + radius * angle.sin()) * PER_MM),
            );
        }
        emit(floor(to.0 * PER_MM), floor(to.1 * PER_MM));
        return;
    }
    straight(from, to, emit);
}

/// Walks the cells a straight move crosses, stepping over one grid line at a
/// time rather than sampling: it visits exactly the cells the segment touches,
/// and the inner loop is a comparison and an addition.
fn straight(from: (f64, f64), to: (f64, f64), mut emit: impl FnMut(i32, i32)) {
    let (x0, y0) = (from.0 * PER_MM, from.1 * PER_MM);
    let (x1, y1) = (to.0 * PER_MM, to.1 * PER_MM);
    let (mut column, mut row) = (floor(x0), floor(y0));
    emit(column, row);
    if !(x0.is_finite() && y0.is_finite() && x1.is_finite() && y1.is_finite()) {
        return;
    }

    let (last_column, last_row) = (floor(x1), floor(y1));
    let (dx, dy) = (x1 - x0, y1 - y0);
    let step_column = if dx > 0.0 { 1 } else { -1 };
    let step_row = if dy > 0.0 { 1 } else { -1 };
    // How far along the move the next grid line each way lies, as a fraction
    // of the whole move, and how far apart the ones after it are.
    let mut next_column = boundary(x0, dx, column);
    let mut next_row = boundary(y0, dy, row);
    let along_column = (1.0 / dx).abs();
    let along_row = (1.0 / dy).abs();

    let mut steps = 0;
    while (column != last_column || row != last_row) && steps < MAX_CELLS {
        if next_column < next_row {
            column += step_column;
            next_column += along_column;
        } else {
            row += step_row;
            next_row += along_row;
        }
        emit(column, row);
        steps += 1;
    }
}

/// How far along a move its first grid line lies, as a fraction of the move.
fn boundary(at: f64, delta: f64, cell: i32) -> f64 {
    if delta == 0.0 {
        return f64::INFINITY;
    }
    let edge = if delta > 0.0 {
        cell as f64 + 1.0
    } else {
        cell as f64
    };
    (edge - at) / delta
}

/// Centre, radius, opening angle and swept angle of an arc, or `None` where the
/// `I`/`J` offsets do not describe one.
fn turn(from: (f64, f64), to: (f64, f64), arc: Arc) -> Option<((f64, f64), f64, f64, f64)> {
    let centre = (from.0 + arc.i, from.1 + arc.j);
    let radius = arc.i.hypot(arc.j);
    if !radius.is_finite() || radius <= 0.0 {
        return None;
    }
    let start = (from.1 - centre.1).atan2(from.0 - centre.0);
    let end = (to.1 - centre.1).atan2(to.0 - centre.0);
    let mut sweep = if arc.clockwise {
        start - end
    } else {
        end - start
    };
    if !sweep.is_finite() {
        return None;
    }
    // Both angles come out of `atan2` in (-π, π], so their difference is
    // already inside one turn and a single wrap settles it. A full circle
    // arrives as zero, which is the one case that has to become a whole turn
    // rather than nothing.
    if sweep <= 0.0 {
        sweep += TAU;
    }
    Some((
        centre,
        radius,
        start,
        if arc.clockwise { -sweep } else { sweep },
    ))
}

fn samples(length: f64) -> usize {
    if !length.is_finite() || length <= STEP {
        return 1;
    }
    ((length / STEP).ceil() as usize).clamp(1, MAX_CELLS)
}

/// `as` saturates, so a coordinate that is not a number lands in a corner of
/// the grid instead of wrapping into somebody else's cell.
fn floor(grid: f64) -> i32 {
    grid.floor() as i32
}

/// A cell as one number. Sixteen bits an axis reaches nearly ten metres either
/// way at [`CELL`], which is further than any printer moves, and halves what a
/// file's answer costs to keep.
fn key(column: i32, row: i32) -> u32 {
    let narrow = |value: i32| value.clamp(i16::MIN as i32, i16::MAX as i32) as u16 as u32;
    narrow(column) << 16 | narrow(row)
}

/// The cells a set of paths passes through.
///
/// Kept sorted once [`Cells::settle`] has run, which is what makes membership a
/// binary search and the difference of two layers a single merge.
#[derive(Clone, Debug, Default)]
pub struct Cells {
    keys: Vec<u32>,
}

impl Cells {
    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }

    pub fn len(&self) -> usize {
        self.keys.len()
    }

    /// Records the cells a move passes through.
    pub fn draw(&mut self, from: (f64, f64), to: (f64, f64), arc: Option<Arc>) {
        let keys = &mut self.keys;
        cells(from, to, arc, |cell| keys.push(cell));
    }

    /// Orders the cells so the set can be searched and compared.
    pub fn settle(&mut self) {
        self.keys.sort_unstable();
        self.keys.dedup();
    }

    /// Empties the set but keeps what it allocated, so reading a layer costs
    /// nothing the layer before it has already paid for.
    pub fn clear(&mut self) {
        self.keys.clear();
    }

    /// True when this set holds a cell. Only meaningful once [`Cells::settle`]
    /// has run.
    pub fn has(&self, cell: u32) -> bool {
        self.keys.binary_search(&cell).is_ok()
    }

    /// True when a point falls in a cell this set holds.
    pub fn holds(&self, x: f64, y: f64) -> bool {
        self.has(key(floor(x * PER_MM), floor(y * PER_MM)))
    }

    /// The cells of this set that `other` does not hold. Both must be settled.
    pub fn without(&self, other: &Cells) -> Cells {
        let mut keys = Vec::new();
        let mut at = 0;
        for &cell in &self.keys {
            while at < other.keys.len() && other.keys[at] < cell {
                at += 1;
            }
            if other.keys.get(at) != Some(&cell) {
                keys.push(cell);
            }
        }
        keys.shrink_to_fit();
        Cells { keys }
    }

    /// Hands the cells over and leaves this set empty, so a layer's footprint
    /// can become the layer below's without copying it.
    pub fn take(&mut self) -> Cells {
        Cells {
            keys: std::mem::take(&mut self.keys),
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    fn cells_of(from: (f64, f64), to: (f64, f64), arc: Option<Arc>) -> Cells {
        let mut cells = Cells::default();
        cells.draw(from, to, arc);
        cells.settle();
        cells
    }

    #[test]
    fn a_straight_move_covers_every_cell_it_crosses() {
        let cells = cells_of((0.0, 0.0), (3.0, 0.0), None);
        for step in 0..30 {
            let x = step as f64 / 10.0;
            assert!(cells.holds(x, 0.0), "gap at {x}");
        }
        assert!(!cells.holds(1.5, 1.0));
    }

    #[test]
    fn a_full_circle_arc_covers_the_ring_and_not_its_chord() {
        // A ring drawn as one G3 back to its own start: the chord is a point,
        // so a tracer that took it would report almost nothing covered.
        let arc = Some(Arc {
            i: 5.0,
            j: 0.0,
            clockwise: false,
        });
        let cells = cells_of((0.0, 0.0), (0.0, 0.0), arc);
        for step in 0..36 {
            let angle = TAU * step as f64 / 36.0;
            let (x, y) = (5.0 + 5.0 * angle.cos(), 5.0 * angle.sin());
            assert!(cells.holds(x, y), "gap at {angle}");
        }
        assert!(
            !cells.holds(5.0, 0.0),
            "the middle of the ring is not on it"
        );
    }

    #[test]
    fn an_arc_turns_the_way_its_command_says() {
        let centre = Arc {
            i: 0.0,
            j: 1.0,
            clockwise: false,
        };
        // Half a circle from the bottom of it to the top: turning the way the
        // angle increases passes the right-hand side, the other way the left.
        let widdershins = cells_of((0.0, 0.0), (0.0, 2.0), Some(centre));
        let clockwise = cells_of(
            (0.0, 0.0),
            (0.0, 2.0),
            Some(Arc {
                clockwise: true,
                ..centre
            }),
        );
        assert!(widdershins.holds(1.0, 1.0));
        assert!(!widdershins.holds(-1.0, 1.0));
        assert!(clockwise.holds(-1.0, 1.0));
        assert!(!clockwise.holds(1.0, 1.0));
    }

    #[test]
    fn the_same_path_gives_the_same_cells_whichever_end_it_started_from() {
        let out = cells_of((0.0, 0.0), (7.3, 2.9), None);
        let back = cells_of((7.3, 2.9), (0.0, 0.0), None);
        assert_eq!(out.keys, back.keys);
    }

    #[test]
    fn a_difference_keeps_only_what_the_other_set_misses() {
        let mine = cells_of((0.0, 0.0), (3.0, 0.0), None);
        let theirs = cells_of((0.0, 0.0), (1.0, 0.0), None);
        let left = mine.without(&theirs);
        assert!(!left.holds(0.5, 0.0));
        assert!(left.holds(2.5, 0.0));
    }

    #[test]
    fn a_move_no_printer_could_make_is_not_an_allocation() {
        let mut cells = Cells::default();
        cells.draw((0.0, 0.0), (1e18, 1e18), None);
        cells.draw((0.0, 0.0), (f64::NAN, f64::INFINITY), None);
        assert!(cells.len() <= MAX_CELLS * 2 + 4);
    }
}
