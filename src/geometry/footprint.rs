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

use std::f64::consts::{FRAC_PI_2, TAU};

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

/// Cells one move may be cut into. A move longer than a bed is not a move, and
/// a corrupt coordinate must not turn into an allocation. A bed is 600 mm and
/// the finest grid anything here asks for is [`Grid::FINEST`], so this is more
/// than twice what a real move can need.
///
/// A move that wants more has its path [`Trace::Refused`] instead, which costs
/// two cells rather than 32768 and cannot be mistaken for a path that was
/// followed.
const MAX_CELLS: usize = 32768;

/// How coarsely the plane is quantised.
///
/// The wall-stacking test is happy at [`CELL`], because a cell is the unit of
/// "these two beads overlap". [`surface`](crate::zaa::surface) is not: it measures
/// the strip a layer leaves exposed, which on a 20° slope is half a
/// millimetre, and a grid that cannot express a distance under a cell reads
/// every slope steeper than that as the same one. So the resolution travels
/// with the cells rather than being a constant everything shares.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Grid {
    cell: f64,
    per_mm: f64,
}

impl Default for Grid {
    fn default() -> Self {
        Self::of(CELL)
    }
}

impl Grid {
    /// The finest grid worth asking for, in mm.
    ///
    /// A bead is around 0.42 mm wide, so a tenth of that is already finer than
    /// anything the extrusion itself can express, and the outlines being
    /// measured are written on a 1 µm coordinate grid. Below this the answer
    /// stops improving and the flood fill that separates inside from outside
    /// starts to see the 0.04 mm gap a slicer leaves at a seam as a way in.
    pub const FINEST: f64 = 0.05;

    pub fn of(cell: f64) -> Self {
        Self::held(cell, CELL)
    }

    /// A grid at `cell`, no finer than [`Grid::FINEST`] and no coarser than
    /// `coarsest`. Anything that is not a length at all is the default.
    fn held(cell: f64, coarsest: f64) -> Self {
        let cell = if cell.is_finite() && cell > 0.0 {
            cell.clamp(Self::FINEST, coarsest.max(Self::FINEST))
        } else {
            CELL
        };
        Self {
            cell,
            per_mm: 1.0 / cell,
        }
    }

    pub fn cell(self) -> f64 {
        self.cell
    }

    /// Cells a window is assumed to add around the span it was asked for.
    ///
    /// The caller measures a path and then rasterises it, which reaches half
    /// a cell past either end, and keeps a clear ring around the result. A
    /// grid picked for the bare span therefore lands just over budget and the
    /// window is refused: measured on a 60 mm cone at a million cells, the
    /// span asked for exactly 1000 cells a side and the window wanted 1004,
    /// and the whole part went unfollowed.
    const SLACK: f64 = 8.0;

    /// The finest grid a print this wide fits into `budget` cells.
    ///
    /// Resolution costs area, and the area is a bed rather than a file, so
    /// what a small part can afford a bed-filling one cannot. Spending the
    /// same budget either way keeps the memory this tool uses flat and gives
    /// the resolution to whoever has room for it.
    ///
    /// The budget is the only ceiling. [`CELL`] is not one: it is the
    /// tolerance of "these two beads overlap" and answers a different
    /// question, and held to it a span over about 422 mm square cannot be
    /// fitted into a two-million-cell budget at all — a 450 mm square layer
    /// wants 2.27M cells at 0.3 mm and a 600x300 mm one 2.02M, so the caller
    /// refused every layer of them and the whole transform silently did
    /// nothing. Past that the resolution gives way instead: at two million
    /// cells, 0.302 mm at 600x300 mm, 0.320 mm at 450 mm square and 0.427 mm
    /// at 600 mm square, which is the largest bed there is.
    pub fn for_span(width: f64, depth: f64, budget: usize) -> Self {
        let (width, depth) = (width.max(0.0), depth.max(0.0));
        let budget = budget as f64 - Self::SLACK * Self::SLACK;
        if !(width * depth).is_finite() || width * depth <= 0.0 || budget <= 0.0 {
            return Self::default();
        }
        // Cells across the window are `span / cell + SLACK` on each axis, so
        // the budget is a quadratic in cells per mm: `wd·n² + s(w+d)·n + s²`.
        let (a, b) = (width * depth, Self::SLACK * (width + depth));
        let per_mm = (b.mul_add(-1.0, (b * b + 4.0 * a * budget).sqrt())) / (2.0 * a);
        Self::held(1.0 / per_mm, f64::INFINITY)
    }

    /// The cell a point in the plane falls in.
    pub fn at(self, x: f64, y: f64) -> (i32, i32) {
        (floor(x * self.per_mm), floor(y * self.per_mm))
    }

    /// Sampling step along an arc. Half a cell, so the curve moves less than a
    /// cell between two samples and the segment between them is short enough
    /// to walk, which is what makes the cells a property of the path rather
    /// than of where the sampling happened to start.
    fn step(self) -> f64 {
        self.cell / 2.0
    }
}

/// The centre and direction of a `G2`/`G3`, taken from its `I`/`J` offsets.
#[derive(Clone, Copy, Debug)]
pub struct Arc {
    pub i: f64,
    pub j: f64,
    pub clockwise: bool,
}

/// What a walk over a move's path did.
///
/// A move too long to rasterise at this cell size, or one whose ends are not
/// coordinates, has its **path refused**: the cells between its ends are not
/// visited, and the caller is told. Everything downstream reads a cell that is
/// missing as "nothing was printed there" — in [`brick`](crate::brick) that is a
/// column capped where it carries on, or left uncapped where it ends, and in
/// [`zaa`](crate::zaa) a hole in the layer's coverage — so a trace cut short in
/// the middle is a lie no caller can see through, where a refusal is one every
/// caller can.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Trace {
    /// Every cell the move's path passes through was visited.
    Whole,
    /// The path was not followed. Only the two cells the move's ends stand in
    /// were visited — that is where the nozzle demonstrably was, and a caller
    /// sizing a window off the footprint has to see it — or nothing at all
    /// where the ends are not coordinates either.
    ///
    /// No printer makes such a move: warn and carry on, never fail — whatever
    /// file it came out of is already being printed.
    Refused,
}

/// Calls `visit` once for each grid cell the path of a move passes through, in
/// order and without repeating the one before.
///
/// Arcs are followed round rather than cut across. A slicer with arc fitting on
/// draws a whole ring as two or three `G2`s, and taking their chords would say
/// the ring covers nothing at all.
///
/// A move whose path cannot be followed faithfully is refused rather than cut
/// short — see [`Trace`].
pub fn cells(
    grid: Grid,
    from: (f64, f64),
    to: (f64, f64),
    arc: Option<Arc>,
    mut visit: impl FnMut(u32),
) -> Trace {
    let per_mm = grid.per_mm;
    let (start, end) = (scale(from, per_mm), scale(to, per_mm));
    // Before a single cell is emitted. `floor` saturates, so a coordinate that
    // is not a number quantises into a corner of the grid or into (0, 0) —
    // cells a real path also prints in, and so indistinguishable from a wall
    // that was genuinely laid there.
    if !finite(start) || !finite(end) {
        return Trace::Refused;
    }

    let mut last = None;
    let mut emit = |column: i32, row: i32| {
        let cell = key(column, row);
        if last != Some(cell) {
            last = Some(cell);
            visit(cell);
        }
    };

    if let Some(arc) = arc
        && let Some(curve) = turn(from, to, arc)
    {
        return round(grid, (start, end), curve, &mut emit);
    }
    let at = (floor(start.0), floor(start.1));
    if reach(at, end) > MAX_CELLS as u64 {
        return ends(start, end, &mut emit);
    }
    emit(at.0, at.1);
    walk(start, end, at, &mut emit);
    Trace::Whole
}

/// Refuses a move's path, keeping the two cells its ends stand in.
///
/// Those two are not a trace and are not offered as one, but they are the one
/// thing about the move that is known exactly. A walk cut short at [`MAX_CELLS`]
/// used to leave a trail heading off towards a coordinate no printer could
/// reach, and something does read that: a caller measuring how far a layer
/// reaches sizes its window off the footprint, and has to refuse a layer like
/// this rather than fit a window to a part that is not there.
fn ends(start: (f64, f64), end: (f64, f64), emit: &mut impl FnMut(i32, i32)) -> Trace {
    emit(floor(start.0), floor(start.1));
    emit(floor(end.0), floor(end.1));
    Trace::Refused
}

/// Follows an arc round, walking between its samples rather than jumping.
///
/// Half-cell samples bound how far the curve moves between two of them, but not
/// which cells it went through on the way: two samples can be diagonal
/// neighbours, and the cell the curve clipped at their shared corner is then
/// never visited. That put an arc-fitted wall and the same wall written out as
/// segments in the same set as different footprints. Walking each pair costs a
/// step or two and the two agree. The chord walked runs inside the curve by its
/// sagitta, which at half a cell between samples is a hundredth of a cell on a
/// 1 mm radius and less on anything wider.
fn round(
    grid: Grid,
    (start, end): ((f64, f64), (f64, f64)),
    (centre, radius, opening, sweep): ((f64, f64), f64, f64, f64),
    emit: &mut impl FnMut(i32, i32),
) -> Trace {
    let per_mm = grid.per_mm;
    let Some(steps) = samples(grid, radius * sweep.abs()) else {
        return ends(start, end, emit);
    };
    // A circle whose far side is past what a coordinate can hold. Both extremes
    // are inside this one, since the centre and the radius are both finite.
    if !finite(scale(
        (centre.0.abs() + radius, centre.1.abs() + radius),
        per_mm,
    )) {
        return ends(start, end, emit);
    }
    // The end of a `G2` is not always on its own circle — a slicer rounds it to
    // the micron, and nothing says a corrupt one is anywhere near it — so the
    // hop that closes the arc is measured like a straight move.
    let finish = on_circle(centre, radius, opening + sweep, per_mm);
    if reach((floor(finish.0), floor(finish.1)), end) > MAX_CELLS as u64 {
        return ends(start, end, emit);
    }

    let mut at = start;
    let mut cell = (floor(at.0), floor(at.1));
    emit(cell.0, cell.1);
    for step in 1..=steps {
        let angle = opening + sweep * step as f64 / steps as f64;
        let next = on_circle(centre, radius, angle, per_mm);
        walk(at, next, cell, &mut *emit);
        cell = (floor(next.0), floor(next.1));
        at = next;
    }
    walk(at, end, cell, emit);
    Trace::Whole
}

/// Walks the cells a straight segment crosses in grid coordinates, stepping
/// over one grid line at a time rather than sampling: it visits exactly the
/// cells the segment touches, and the inner loop is a comparison and an
/// addition. The cell the segment starts in is the caller's to emit, and the
/// caller has already counted the steps this takes — an axis only ever moves
/// towards its own end, so the walk arrives in exactly [`reach`] of them.
fn walk(
    (x0, y0): (f64, f64),
    (x1, y1): (f64, f64),
    (mut column, mut row): (i32, i32),
    emit: &mut impl FnMut(i32, i32),
) {
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

    while column != last_column || row != last_row {
        // An axis that has arrived is never stepped again. Without that, a
        // move whose end lands exactly on a grid line — which any coordinate
        // that is a multiple of the cell size does — crosses that line at
        // exactly the end of the move, steps past its own destination, and
        // then walks until the cap: 8193 cells for a 1.5 mm move.
        let across = column != last_column;
        let along = row != last_row;
        if across && (!along || next_column < next_row) {
            column += step_column;
            next_column += along_column;
        } else {
            row += step_row;
            next_row += along_row;
        }
        emit(column, row);
    }
}

/// Grid lines a straight segment from a cell to a point crosses, which is
/// exactly the number of steps [`walk`] takes. Counted before anything is
/// emitted, and in `u64` because the two ends can sit at opposite corners of
/// the `i32` grid.
fn reach((column, row): (i32, i32), (x, y): (f64, f64)) -> u64 {
    u64::from(column.abs_diff(floor(x))) + u64::from(row.abs_diff(floor(y)))
}

/// A point on a circle, in cells.
fn on_circle(centre: (f64, f64), radius: f64, angle: f64, per_mm: f64) -> (f64, f64) {
    scale(
        (
            centre.0 + radius * angle.cos(),
            centre.1 + radius * angle.sin(),
        ),
        per_mm,
    )
}

/// A point in mm, in cells.
fn scale((x, y): (f64, f64), per_mm: f64) -> (f64, f64) {
    (x * per_mm, y * per_mm)
}

fn finite((x, y): (f64, f64)) -> bool {
    x.is_finite() && y.is_finite()
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
///
/// The swept angle carries the direction: negative for a `G2`. Anything that
/// has to follow an arc rather than cut across it works from this, so a
/// tracer and a transform always take the same path round.
pub fn turn(from: (f64, f64), to: (f64, f64), arc: Arc) -> Option<((f64, f64), f64, f64, f64)> {
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

/// The smallest box the path of one move fits in, as `[left, front, right,
/// back]` in mm.
///
/// A chord is not a bound. An arc bulges away from the line between its ends
/// by its sagitta, and a slicer with arc fitting on draws a ring as two 180°
/// `G2`s, whose ends share a coordinate — so a box grown from the ends alone
/// reports a span of zero on that axis. What is sized from that span is the
/// surface grid, which then comes out as coarse as it goes or does not fit at
/// all.
///
/// Exact rather than sampled: a circular arc reaches its extremes at its two
/// ends and at whichever of the four compass points its sweep passes.
pub fn extent(from: (f64, f64), to: (f64, f64), arc: Option<Arc>) -> [f64; 4] {
    let mut box_ = [
        from.0.min(to.0),
        from.1.min(to.1),
        from.0.max(to.0),
        from.1.max(to.1),
    ];
    let Some(arc) = arc else {
        return box_;
    };
    let Some((centre, radius, start, sweep)) = turn(from, to, arc) else {
        return box_;
    };
    for (quarter, (cos, sin)) in COMPASS.into_iter().enumerate() {
        // The unit vectors are written out rather than taken from `cos`/`sin`,
        // which are a rounding either side of zero at these angles and would
        // put the box a hair outside the arc it is bounding.
        let angle = FRAC_PI_2 * quarter as f64;
        if !passes(start, sweep, angle) {
            continue;
        }
        let (x, y) = (centre.0 + radius * cos, centre.1 + radius * sin);
        box_ = [
            box_[0].min(x),
            box_[1].min(y),
            box_[2].max(x),
            box_[3].max(y),
        ];
    }
    box_
}

/// East, north, west, south: the four points of a circle that are extreme in
/// one axis, in the order [`extent`] walks their angles.
const COMPASS: [(f64, f64); 4] = [(1.0, 0.0), (0.0, 1.0), (-1.0, 0.0), (0.0, -1.0)];

/// True where a turn opening at `start` and sweeping by `sweep` reaches
/// `angle`. The sign of the sweep is the one [`turn`] returns, negative for a
/// `G2`, so how far the turn has run when it arrives is measured in its own
/// direction.
fn passes(start: f64, sweep: f64, angle: f64) -> bool {
    let turned = if sweep < 0.0 {
        (start - angle).rem_euclid(TAU)
    } else {
        (angle - start).rem_euclid(TAU)
    };
    turned <= sweep.abs()
}

/// Pieces an arc is followed in, or `None` where its path is longer than
/// [`MAX_CELLS`] cells of this grid and so cannot be followed at all. The
/// pieces are half a cell each, which is the bound [`round`] walks between.
fn samples(grid: Grid, length: f64) -> Option<usize> {
    if !length.is_finite() || length > MAX_CELLS as f64 * grid.cell {
        return None;
    }
    let step = grid.step();
    if length <= step {
        return Some(1);
    }
    Some((length / step).ceil() as usize)
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

/// The column and row a packed cell names.
fn unkey(cell: u32) -> (i32, i32) {
    (
        i32::from((cell >> 16) as u16 as i16),
        i32::from((cell & 0xffff) as u16 as i16),
    )
}

/// The cells a set of paths passes through.
///
/// Kept sorted once [`Cells::settle`] has run, which is what makes membership a
/// binary search, the difference of two layers a single merge, and two settled
/// layers comparable for equality in one pass.
#[derive(Clone, Debug, Default)]
pub struct Cells {
    grid: Grid,
    keys: Vec<u32>,
    refused: usize,
}

/// Two sets are equal when they hold the same cells of the same plane. How many
/// moves were refused is a note about the reading rather than part of what was
/// read: [`surface`](crate::zaa::surface) skips a layer whose neighbours match
/// it, and counting refusals in would make two identical footprints differ.
impl PartialEq for Cells {
    fn eq(&self, other: &Self) -> bool {
        self.grid == other.grid && self.keys == other.keys
    }
}

impl Cells {
    /// An empty set quantised to `grid` rather than to the shared [`CELL`].
    pub fn on(grid: Grid) -> Self {
        Self {
            grid,
            keys: Vec::new(),
            refused: 0,
        }
    }

    pub fn grid(&self) -> Grid {
        self.grid
    }

    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }

    pub fn len(&self) -> usize {
        self.keys.len()
    }

    /// Moves this set could not take, because their paths could not be
    /// followed faithfully — see [`Trace`]. Anything but zero means the
    /// footprint has holes in it that are not absences of material, which is
    /// worth a line of warning to whoever is about to print it.
    pub fn refused(&self) -> usize {
        self.refused
    }

    /// Records the cells a move passes through, and says whether it could.
    pub fn draw(&mut self, from: (f64, f64), to: (f64, f64), arc: Option<Arc>) -> Trace {
        let (grid, keys) = (self.grid, &mut self.keys);
        let traced = cells(grid, from, to, arc, |cell| keys.push(cell));
        self.refused += usize::from(traced == Trace::Refused);
        traced
    }

    /// Takes in cells already quantised to this grid, so a caller that has
    /// just walked a path can keep it without walking it again.
    pub fn absorb(&mut self, keys: &[u32]) {
        self.keys.extend_from_slice(keys);
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
        self.refused = 0;
    }

    /// True when this set holds a cell. Only meaningful once [`Cells::settle`]
    /// has run.
    pub fn has(&self, cell: u32) -> bool {
        self.keys.binary_search(&cell).is_ok()
    }

    /// True when a point falls in a cell this set holds.
    pub fn holds(&self, x: f64, y: f64) -> bool {
        let (column, row) = self.grid.at(x, y);
        self.has(key(column, row))
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
        Cells {
            grid: self.grid,
            keys,
            refused: self.refused,
        }
    }

    /// Hands the cells over and leaves this set empty, so a layer's footprint
    /// can become the layer below's without copying it.
    pub fn take(&mut self) -> Cells {
        Cells {
            grid: self.grid,
            keys: std::mem::take(&mut self.keys),
            refused: std::mem::take(&mut self.refused),
        }
    }

    /// The column and row of every cell in the set.
    pub fn iter(&self) -> impl Iterator<Item = (i32, i32)> + '_ {
        self.keys.iter().copied().map(unkey)
    }

    /// The smallest box holding every cell, as `[left, bottom, right, top]` in
    /// grid coordinates and inclusive at both ends. `None` for an empty set.
    ///
    /// Measured rather than derived from the sort order: cells pack the two
    /// signed axes into one unsigned word, so the ordering that makes
    /// membership a binary search is not the ordering of either axis.
    pub fn bounds(&self) -> Option<[i32; 4]> {
        self.iter().fold(None, |box_, (column, row)| {
            Some(box_.map_or([column, row, column, row], |had: [i32; 4]| {
                [
                    had[0].min(column),
                    had[1].min(row),
                    had[2].max(column),
                    had[3].max(row),
                ]
            }))
        })
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
        assert_eq!(cells.draw((0.0, 0.0), (1e18, 1e18), None), Trace::Refused);
        assert_eq!(
            cells.draw((0.0, 0.0), (f64::NAN, f64::INFINITY), None),
            Trace::Refused
        );
        assert_eq!(cells.refused(), 2);
        // The first keeps the two cells its ends stand in and not the path
        // between them; the second has no ends worth keeping. Cut short at the
        // cap, the first used to leave 32768 keys behind.
        assert_eq!(cells.len(), 2);
        assert!(
            cells.keys.capacity() <= 8,
            "a refused move allocated {} keys",
            cells.keys.capacity()
        );
    }

    /// A move longer than the walk can follow used to come back cut short at
    /// [`MAX_CELLS`], with nothing to say so. Downstream a cell that is not in
    /// the set means no material, so the tail of the move became a stretch of
    /// wall the layer above does not cover — a column capped where it carries
    /// on, or a hole in a layer's coverage. Refused, a caller can tell.
    #[test]
    fn a_move_too_long_for_the_grid_is_refused_whole_and_not_cut_short() {
        let mut cells = Cells::default();
        // A bed's width and its depth, which is longer than any real move.
        assert_eq!(cells.draw((0.0, 0.0), (600.0, 600.0), None), Trace::Whole);
        let bed = cells.len();
        assert!(bed > 4000, "{bed} cells for 1200 mm of travel");
        assert_eq!(cells.refused(), 0);

        let far = 2.0 * MAX_CELLS as f64 * CELL;
        assert_eq!(cells.draw((0.0, 0.0), (far, 0.0), None), Trace::Refused);
        // The path is refused, the two cells its ends stand in are kept.
        assert_eq!(cells.len(), bed + 2, "a refused path was walked anyway");
        assert_eq!(cells.refused(), 1);

        // An arc is bounded by the path it follows, not by its ends: a ring
        // 10 km round arrives as a `G3` back where it started.
        let ring = |radius: f64| {
            Some(Arc {
                i: radius,
                j: 0.0,
                clockwise: false,
            })
        };
        assert_eq!(
            cells.draw((0.0, 0.0), (0.0, 0.0), ring(5000.0)),
            Trace::Refused
        );
        assert!(cells.len() < bed + 8, "a refused arc was followed anyway");
        assert_eq!(cells.refused(), 2);
        let kept = cells.len();
        assert_eq!(cells.draw((0.0, 0.0), (0.0, 0.0), ring(5.0)), Trace::Whole);
        assert!(cells.len() > kept + 100, "a 31 mm ring is 105 cells");
    }

    /// `floor` saturates, so a coordinate that is not a number lands in a
    /// corner of the grid or in (0, 0) — cells a real path also prints in, and
    /// so read downstream as material that was genuinely laid there. The check
    /// has to come before the first cell is emitted, not after it.
    #[test]
    fn a_coordinate_that_is_not_a_number_marks_no_cell_at_all() {
        let ring = Some(Arc {
            i: 1.0,
            j: 0.0,
            clockwise: false,
        });
        for (from, to, arc) in [
            ((f64::NAN, 0.0), (1.0, 1.0), None),
            ((f64::INFINITY, 0.0), (1.0, 1.0), None),
            ((0.0, f64::NEG_INFINITY), (1.0, 1.0), None),
            ((1.0, 1.0), (f64::NAN, 0.0), None),
            ((1.0, 1.0), (0.0, f64::INFINITY), None),
            ((f64::NAN, 0.0), (1.0, 1.0), ring),
            ((0.0, 0.0), (f64::INFINITY, f64::INFINITY), ring),
        ] {
            let mut cells = Cells::default();
            assert_eq!(
                cells.draw(from, to, arc),
                Trace::Refused,
                "{from:?} -> {to:?}"
            );
            assert!(cells.is_empty(), "{from:?} -> {to:?} marked a cell");
            assert!(!cells.holds(0.0, 0.0), "{from:?} -> {to:?} marked (0, 0)");
        }
    }

    /// A cell is the unit of "these two beads overlap", so a wall drawn as one
    /// `G2` has to cover what the same wall covers written out as segments.
    /// Samples half a cell apart bound how far the curve moves between two of
    /// them but not what it crossed on the way: where two land diagonally, the
    /// cell the curve clipped at their shared corner was never visited and the
    /// arc-fitted wall read as the thinner one. A walk steps one axis at a
    /// time, so every cell of a traced path touches the one before it.
    #[test]
    fn a_traced_path_never_jumps_a_corner() {
        let arc = |i: f64, j: f64, clockwise: bool| Some(Arc { i, j, clockwise });
        let mut paths = vec![
            ((0.0, 0.0), (3.0, 1.7), None),
            ((0.0, 0.0), (0.0, 2.0), arc(0.0, 1.0, false)),
            ((7.3, 2.9), (4.1, -3.2), arc(-1.6, -3.0, false)),
            ((7.3, 2.9), (4.1, -3.2), arc(-1.6, -3.0, true)),
        ];
        // Rings and quarter turns, offset so they do not line up with the grid
        // they are read on, at radii from under a cell to a part's width.
        for clockwise in [false, true] {
            for radius in [0.11, 0.4, 1.0, 2.35, 5.0, 11.7] {
                let (from, centre) = ((0.13, -0.07), arc(radius, 0.0, clockwise));
                paths.push((from, from, centre));
                paths.push((from, (from.0 + radius, from.1 + radius), centre));
            }
        }
        for (from, to, curve) in paths {
            let mut path = Vec::new();
            let traced = cells(Grid::default(), from, to, curve, |cell| {
                path.push(unkey(cell))
            });
            assert_eq!(traced, Trace::Whole, "{from:?} -> {to:?}");
            for pair in path.windows(2) {
                let ((column, row), (next_column, next_row)) = (pair[0], pair[1]);
                let step = column.abs_diff(next_column) + row.abs_diff(next_row);
                assert_eq!(
                    step, 1,
                    "{:?} jumped to {:?} on {from:?} -> {to:?}",
                    pair[0], pair[1]
                );
            }
        }
    }

    /// A coordinate that is a whole number of cells lands exactly on a grid
    /// line, so the move crosses that line at exactly its own end. Stepping
    /// there anyway takes the walk past its destination, and once past it the
    /// destination is never reached again: a 1.5 mm move drew 8193 cells and
    /// put a streak of them right across the part.
    #[test]
    fn a_move_that_ends_on_a_grid_line_stops_there() {
        // -28.8 is 96 cells exactly, and the move still travels in X.
        let cells = cells_of((-1.507, -28.761), (-0.0, -28.8), None);
        assert!(cells.len() < 12, "{} cells for 1.5 mm", cells.len());
        let [left, bottom, right, top] = cells.bounds().expect("a box");
        assert_eq!((left, right), (-6, 0));
        // -28.8 is the lower edge of row -96, so the whole move is in it.
        assert_eq!((bottom, top), (-96, -96));

        // Both axes on a grid line, and neither, and one that only moves in
        // the axis that lands on one.
        for (from, to) in [
            ((0.0, 0.0), (2.4, 3.0)),
            ((0.3, 0.6), (0.9, 1.2)),
            ((-0.9, 4.5), (-0.9, 0.3)),
            ((5.0, -0.0), (-0.0, 5.0)),
        ] {
            let cells = cells_of(from, to, None);
            let span = ((to.0 - from.0).abs() + (to.1 - from.1).abs()) / CELL;
            assert!(
                (cells.len() as f64) < span + 4.0,
                "{from:?} -> {to:?} drew {} cells",
                cells.len()
            );
        }
    }

    /// The two axes are packed into one word so a set can be searched with a
    /// binary search, which means the sort order is not either axis's order.
    /// A box measured off the first and last key would be wrong the moment a
    /// path crosses the origin.
    #[test]
    fn a_boxes_edges_are_measured_and_not_read_off_the_sort() {
        assert_eq!(Cells::default().bounds(), None);
        let cells = cells_of((-2.0, -1.0), (1.0, 0.5), None);
        let [left, bottom, right, top] = cells.bounds().expect("a box");
        assert_eq!((left, bottom), Grid::default().at(-2.0, -1.0));
        assert_eq!((right, top), Grid::default().at(1.0, 0.5));
        for (column, row) in cells.iter() {
            assert!(column >= left && column <= right, "{column}");
            assert!(row >= bottom && row <= top, "{row}");
        }
    }

    #[test]
    fn a_point_names_the_cell_the_set_would_hold_it_in() {
        let cells = cells_of((0.0, 0.0), (3.0, 0.0), None);
        for step in 0..30 {
            let x = step as f64 / 10.0;
            let (column, row) = Grid::default().at(x, 0.0);
            assert!(cells.iter().any(|cell| cell == (column, row)), "gap at {x}");
        }
    }

    /// A cell finer than the flood fill can separate is worse than useless,
    /// and one coarser than the default answers a question already settled
    /// better elsewhere. Anything a caller cannot state is the default.
    #[test]
    fn a_grid_is_held_between_the_finest_it_can_fill_and_the_default() {
        assert_eq!(Grid::of(0.001).cell(), Grid::FINEST);
        assert_eq!(Grid::of(10.0).cell(), CELL);
        assert_eq!(Grid::of(f64::NAN).cell(), CELL);
        assert_eq!(Grid::of(f64::INFINITY).cell(), CELL);
        assert_eq!(Grid::of(-1.0).cell(), CELL);
        assert_eq!(Grid::of(0.0).cell(), CELL);
        assert_eq!(Grid::default().cell(), CELL);
        assert_eq!(Grid::of(0.1).cell(), 0.1);
    }

    /// The budget is what the window really costs, not what the bare span
    /// would: the window keeps a clear ring around the outlines and the
    /// rasterising reaches past either end. Measured on a 60 mm cone at a
    /// million cells before the slack was allowed for, the span asked for
    /// exactly 1000 cells a side and the window wanted 1004, so every layer
    /// of the part was refused.
    ///
    /// The budget is met at **every** span, large-format beds included. Held
    /// to [`CELL`] the arithmetic ran out at about 422 mm square: a 450 mm
    /// square layer wanted 2.27M cells of the 2M kept and a 600x300 mm one
    /// 2.02M, so both lost the surface transform outright rather than being
    /// followed a little more coarsely.
    #[test]
    fn a_grid_leaves_room_for_the_ring_the_window_keeps_clear() {
        for budget in [10_000, 250_000, 1_000_000, 2_000_000, 4_000_000] {
            for (width, depth) in [
                (60.0, 60.0),
                (180.0, 180.0),
                (256.0, 256.0),
                (20.0, 20.0),
                (300.0, 12.0),
                (450.0, 450.0),
                (600.0, 300.0),
                (600.0, 600.0),
            ] {
                let grid = Grid::for_span(width, depth, budget);
                let cells =
                    (width / grid.cell() + Grid::SLACK) * (depth / grid.cell() + Grid::SLACK);
                assert!(
                    cells <= budget as f64 + 1.0,
                    "{width}x{depth} at {budget} wanted {cells} cells of {}",
                    grid.cell()
                );
            }
        }
        // A bed-scale span gives up resolution rather than the whole answer.
        let wide = Grid::for_span(450.0, 450.0, 2_000_000);
        assert!(wide.cell() > CELL, "{}", wide.cell());
        assert!(wide.cell() < 0.33, "{}", wide.cell());

        // Room to spare goes into resolution, and a part with none keeps the
        // default rather than guessing at a grid it was told nothing about.
        assert!(
            Grid::for_span(60.0, 60.0, 4_000_000).cell()
                < Grid::for_span(180.0, 180.0, 4_000_000).cell()
        );
        assert_eq!(Grid::for_span(1000.0, 1000.0, 1).cell(), CELL);
        assert_eq!(Grid::for_span(60.0, 60.0, 0).cell(), CELL);
        assert_eq!(Grid::for_span(0.0, 60.0, 1_000_000).cell(), CELL);
        assert_eq!(Grid::for_span(f64::INFINITY, 60.0, 1_000_000).cell(), CELL);
    }

    /// An arc bulges away from its own chord, and a ring drawn as two 180°
    /// `G2`s has ends that share a coordinate — so a box grown from the ends
    /// reports no span at all on that axis, and the grid sized from it comes
    /// out as coarse as it goes.
    #[test]
    fn an_arcs_box_holds_the_bulge_and_not_just_its_ends() {
        let arc = |i: f64, j: f64, clockwise: bool| Arc { i, j, clockwise };

        // Half a circle of radius 1 from the bottom of it to the top. Both
        // ends sit on x = 0; the sweep is the whole of one side.
        let widdershins = extent((0.0, 0.0), (0.0, 2.0), Some(arc(0.0, 1.0, false)));
        assert_eq!(widdershins, [0.0, 0.0, 1.0, 2.0]);
        let clockwise = extent((0.0, 0.0), (0.0, 2.0), Some(arc(0.0, 1.0, true)));
        assert_eq!(clockwise, [-1.0, 0.0, 0.0, 2.0]);

        // A whole ring arrives as a `G3` back to its own start, and reaches
        // every compass point of it.
        let ring = extent((0.0, 0.0), (0.0, 0.0), Some(arc(5.0, 0.0, false)));
        assert_eq!(ring, [0.0, -5.0, 10.0, 5.0]);

        // A quarter turn passing no compass point is its own two ends.
        let quarter = extent((1.0, 0.0), (0.0, 1.0), Some(arc(-1.0, 0.0, false)));
        assert_eq!(quarter, [0.0, 0.0, 1.0, 1.0]);

        // A straight move, and an arc whose offsets describe no circle, are
        // both bounded by their ends alone.
        let ends = [-2.0, -1.0, 3.0, 4.0];
        assert_eq!(extent((3.0, -1.0), (-2.0, 4.0), None), ends);
        let degenerate = extent((3.0, -1.0), (-2.0, 4.0), Some(arc(0.0, 0.0, false)));
        assert_eq!(degenerate, ends);
    }

    /// Every cell the path really crosses has to lie inside the box measured
    /// for it, or the window sized from that box cannot hold the layer.
    #[test]
    fn an_arcs_box_holds_every_cell_the_arc_draws() {
        let grid = Grid::default();
        let arc = |i: f64, j: f64, clockwise: bool| Arc { i, j, clockwise };
        for clockwise in [false, true] {
            for (from, to, curve) in [
                ((0.0, 0.0), (0.0, 2.0), arc(0.0, 1.0, clockwise)),
                ((0.0, 0.0), (0.0, 0.0), arc(5.0, 0.0, clockwise)),
                ((7.3, 2.9), (4.1, -3.2), arc(-1.6, -3.0, clockwise)),
            ] {
                let [left, front, right, back] = extent(from, to, Some(curve));
                for (column, row) in cells_of(from, to, Some(curve)).iter() {
                    // A cell is named by its lower-left corner, so one holding
                    // a point of the arc starts up to a cell below the box and
                    // never begins past its far edge.
                    let (x, y) = (
                        f64::from(column) * grid.cell(),
                        f64::from(row) * grid.cell(),
                    );
                    assert!(
                        x >= left - grid.cell() - 1e-9 && x <= right + 1e-9,
                        "{x} outside {left}..{right}"
                    );
                    assert!(
                        y >= front - grid.cell() - 1e-9 && y <= back + 1e-9,
                        "{y} outside {front}..{back}"
                    );
                }
            }
        }
    }

    /// Two sets on different grids describe different planes, so a caller
    /// comparing them has to be able to tell.
    #[test]
    fn a_set_carries_the_grid_it_was_drawn_on() {
        let fine = Cells::on(Grid::of(0.1));
        assert_eq!(fine.grid(), Grid::of(0.1));
        assert_ne!(fine.grid(), Grid::default());
        assert_eq!(Cells::default().grid(), Grid::default());

        // The same path drawn twice as finely holds more cells.
        let mut coarse = Cells::default();
        coarse.draw((0.0, 0.0), (3.0, 0.0), None);
        let mut fine = Cells::on(Grid::of(0.1));
        fine.draw((0.0, 0.0), (3.0, 0.0), None);
        assert!(
            fine.len() > coarse.len(),
            "{} vs {}",
            fine.len(),
            coarse.len()
        );
    }
}
