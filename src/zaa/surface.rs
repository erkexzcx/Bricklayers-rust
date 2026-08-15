//! Where the model's own surface sits inside a layer, worked out from the
//! layers either side of it.
//!
//! A slicer takes its cross-section through the **middle** of a layer, so a
//! layer's outline is where the surface passes `plane - height/2` and the next
//! layer's outline is where it passes `plane + height/2`. Between the two —
//! the strip of a layer that nothing is printed over, which is exactly what
//! the slicer labels a top surface — the surface climbs from one to the other.
//! Following it there instead of laying the whole strip flat is what turns a
//! staircase into a ramp.
//!
//! Nothing here needs the model. The two outlines are already in the G-code,
//! as the cells each layer's extrusions pass through, and a linear climb
//! across the strip reproduces a flat slope **exactly** — which is the case
//! that stair-steps in the first place. What it cannot reproduce is a surface
//! that curves sharply inside one strip, and there it errs toward the plane.
//!
//! Two things have to be told apart from a slope, and both fall out of the
//! same arithmetic rather than being special-cased:
//!
//! - **A flat top.** Nothing is printed above it anywhere, so the strip has no
//!   far edge and no climb can be measured. Its width comes out unbounded and
//!   the rise fades to nothing.
//! - **A ledge with a wall standing on it.** The strip has a far edge, but the
//!   *near* edge is a vertical face rather than the surface crossing a plane,
//!   so there is no ramp to continue. The layer below says which it is: under
//!   a slope it reaches a strip's width further out, and under a vertical wall
//!   it stops in the same place.

use crate::geometry::{Cells, Grid};

/// Cost of stepping to a neighbour along an axis, across a corner, and by a
/// knight's move, in a 5-7-11 chamfer distance transform.
///
/// The trio is the standard integer approximation to Euclidean distance over a
/// 5×5 neighbourhood, and it is about 2% out at worst against the 6% of the
/// 3-4 pair over a 3×3 one. The rise is read off these distances, so a
/// direction error becomes an error in height.
const NEAR: u16 = 5;
const FAR: u16 = 7;
const LEAP: u16 = 11;

/// Chamfer units in one cell, so a distance in cells is `value / SPAN`.
const SPAN: f64 = NEAR as f64;

/// How much of a strip the layer below has to reach past this one before the
/// surface counts as fully sloped.
///
/// The layer below's own tread and this layer's strip are each read off the
/// same grid, and the tread is a **difference** of two of those
/// distances while the strip is a **sum** of them: the difference can come out
/// at zero and the sum cannot come out under a cell. Comparing them one for
/// one therefore reads a uniform slope as a partial one, and the worse the
/// finer the slope. Measured on a 60 mm spherical cap before this: the mean
/// came out at **0.368** where the geometry says 1.0, so the transform was
/// delivering nearer a third of the smoothing it had measured.
///
/// Half a strip is the grid's own resolution stated as a ratio. It leaves the
/// two cases this test exists for untouched, because both of them put the
/// layer below in exactly the same place as this one and so read zero however
/// generous the gauge is.
const SLOPE_MARGIN: f64 = 0.5;

/// A cell the transform never reached.
const UNREACHED: u16 = u16::MAX;

/// What the flood fill made of a cell.
///
/// "Is this cell within the outline" and "was anything printed in it" are
/// different questions, and answering only the first is what makes a hole
/// indistinguishable from the interior of a part: sparse infill leaves that
/// interior mostly empty, so where the material sits cannot separate them
/// either. Keeping both answers costs nothing — a byte a cell is what a
/// `bool` already took — and the pair of them is what tells a countersink
/// from a hollow column. See [`mouths`].
const OUTSIDE: u8 = 0;
const MATERIAL: u8 = 1;
const HOLLOW: u8 = 2;
/// A pocket the layer above opens wider than this layer does: an upward
/// facing surface with nothing over it, so it is read as air rather than as
/// something printed on.
const MOUTH: u8 = 3;

/// True where a cell is under the layer's own outline, hollow or not.
fn is_inside(state: u8) -> bool {
    state == MATERIAL || state == HOLLOW
}

/// True where the outline encloses a cell and nothing was printed in it.
fn is_hollow(state: u8) -> bool {
    state >= HOLLOW
}

/// How many rings of cells the layer above has to carry a pocket past this
/// layer's, before it counts as opened rather than as the grid's own wobble.
///
/// The rings are counted outward from the pocket, so the first of them is
/// this layer's own wall — the bead ring around the hole, which the layer
/// above has left uncovered — and everything past that is tread. Two is
/// therefore "a cell of tread beyond the wall", which is the narrowest tread
/// this grid can express at all: a distance read off it is quantised to it,
/// and the same bore rasterised on two layers differs by up to a cell
/// wherever a seam or a rounding falls differently. That wobble is what
/// [`Builder::blur`] exists to take back out, and on a 180 mm dome it was the
/// difference between 54105 written moves and 26610. A tread narrower than a
/// cell is invisible here whether it faces inward or outward, so nothing is
/// given up by refusing it, and a hole with no tread at all is a straight
/// bore that must be left exactly as sliced.
const MOUTH_SLACK: usize = 2;

/// Cells kept clear around the layers being compared, so the flood fill that
/// separates the inside of an outline from the outside always has a ring of
/// outside to start from.
const MARGIN: i32 = 2;

/// Most cells one layer's window may cover.
///
/// Every buffer here is one value a cell, so this is what the transform costs
/// in memory whatever the file's size — and, because the grid is chosen to
/// fit rather than fixed, whatever the part's size too. A 20 mm part and a
/// bed-filling one cost the same; only the resolution differs.
///
/// Resolution is the whole of the quality, so what this number buys is
/// measured rather than picked. On a 180 mm spherical cap, weighted over the
/// layers that leave a tread wider than a bead — the only ones with a
/// staircase to remove — 1M cells smoothed 0.139 of half a layer over 25.8%
/// of the surface at 33 MiB, 2M gave 0.186 over 32.7% at 56 MiB, and 4M gave
/// 0.228 over 39.2% at 102 MiB. Anything up to about 70 mm across reaches
/// [`Grid::FINEST`](crate::geometry::Grid::FINEST) here and is unchanged by
/// a larger budget, so this is bought for bed-scale parts alone.
pub const MAX_WINDOW: usize = 2_000_000;

/// How far **past** the reach the rise tapers out, as a share of the reach.
///
/// Two consecutive strips meet at full amplitude and at no other, and that is
/// arithmetic rather than a preference. Layer k's surface ends at `a·h` above
/// its own plane and layer k+1's begins at `b·h` above its plane one layer
/// higher, so they are at the same height only where `a - b = 1`; with both
/// held inside `±h/2` the one solution is `a = +0.5, b = -0.5`. An amplitude
/// scaled by `f` therefore leaves a riser of `(1 - f)·h` at **every** layer
/// boundary it touches. Tapering inside the range being followed does not
/// soften the last step, it trades one step for a band of them: measured
/// against the layer height on a uniform slope with the taper running inward
/// over the last quarter of the reach, the riser left at each boundary was
/// **1.000 h at 1.00°, 0.479 h at 1.15° and 0.008 h at 1.33°** — a whole
/// staircase, at slopes the tool was reporting as followed. The fixture is
/// `two_layers_of_one_slope_meet_without_a_riser_between_them`.
///
/// So the taper runs outward instead. Everything down to the reach — one
/// degree, wherever the layer height puts that in millimetres — is followed
/// at full amplitude and meets exactly, and it fades over the quarter of the
/// reach past that, which is a surface shallower than the tool claims to
/// follow and was flat as sliced anyway. All three of those slopes now leave
/// **0.000 h**.
///
/// The fade cannot simply be dropped. A covered cell measures `up = 0`, so
/// its strip is its own distance to the outside of its layer: one just inside
/// the strip's high edge measures the strip itself and carries the ramp on
/// exactly, while one deep inside the part measures a strip as wide as the
/// part. Fading on the strip's width is what tells those two apart, and it
/// still does — only the width it starts fading at has moved.
const FADE: f64 = 0.25;

/// How far a rise is quantised, as a fraction of a layer height.
///
/// Two hundredths of a layer is a micron on a 0.2 mm layer, which is the grid
/// G-code coordinates are written on, so nothing is lost by keeping a whole
/// layer's answer in one byte a cell.
const STEPS: f64 = 200.0;

/// The rise of one layer's exposed surface above its own plane, cell by cell,
/// as a fraction of the layer height in `-0.5..=0.5`.
#[derive(Clone, Debug, Default)]
pub struct Field {
    grid: Grid,
    left: i32,
    bottom: i32,
    width: usize,
    height: usize,
    rise: Vec<i8>,
    /// Cells this layer covers that the layer above does not, which is the
    /// only place a bead may be moved: everywhere else something is printed
    /// on top of it.
    open: Vec<bool>,
    /// True where no exposed cell rises at all, so a caller can skip the layer
    /// without looking anything up.
    flat: bool,
}

impl Field {
    /// True where nothing on this layer is to be followed.
    pub fn is_flat(&self) -> bool {
        self.flat
    }

    /// True where a point is on this layer's surface with nothing printed over
    /// it. A bead anywhere else has to stay on the plane whatever the surface
    /// is doing, because the next layer is laid against it.
    pub fn is_open(&self, x: f64, y: f64) -> bool {
        if self.width == 0 {
            return false;
        }
        let (column, row) = self.grid.at(x, y);
        match self.index(column - self.left, row - self.bottom) {
            Some(at) => self.open[at],
            None => false,
        }
    }

    /// How far the surface stands above the plane at a point, as a fraction of
    /// the layer's height.
    ///
    /// Read between the four cells around the point rather than off the one it
    /// lands in. A strip is a handful of cells across, so a value per cell is a
    /// staircase of its own — one that would then be written out as a Z move
    /// per cell instead of as the straight ramp it is approximating.
    pub fn at(&self, x: f64, y: f64) -> f64 {
        if self.flat || self.width == 0 || !(x.is_finite() && y.is_finite()) {
            return 0.0;
        }
        // Cell centres, so the value of a cell is read at its middle and the
        // interpolation between two of them is even.
        let column = x / self.grid.cell() - 0.5 - f64::from(self.left);
        let row = y / self.grid.cell() - 0.5 - f64::from(self.bottom);
        let (left, bottom) = (column.floor(), row.floor());
        let (across, up) = (column - left, row - bottom);
        let corner = |dx: f64, dy: f64| {
            let (column, row) = (left + dx, bottom + dy);
            // Clamped rather than treated as zero: the ring outside the window
            // is already flat, and a hard zero would put a step at its edge.
            let column = (column.max(0.0) as usize).min(self.width - 1);
            let row = (row.max(0.0) as usize).min(self.height - 1);
            f64::from(self.rise[row * self.width + column]) / STEPS
        };
        let lower = corner(0.0, 0.0) * (1.0 - across) + corner(1.0, 0.0) * across;
        let upper = corner(0.0, 1.0) * (1.0 - across) + corner(1.0, 1.0) * across;
        lower * (1.0 - up) + upper * up
    }

    fn index(&self, column: i32, row: i32) -> Option<usize> {
        let column = usize::try_from(column).ok()?;
        let row = usize::try_from(row).ok()?;
        (column < self.width && row < self.height).then_some(row * self.width + column)
    }

    fn clear(&mut self) {
        self.rise.clear();
        self.open.clear();
        self.width = 0;
        self.height = 0;
        self.flat = true;
    }
}

/// Working memory for building one [`Field`] after another.
///
/// A layer's window is the same size as the layer before it, near enough, so
/// every buffer here is filled and refilled rather than allocated per layer —
/// a file with ten thousand layers would otherwise zero a few megabytes ten
/// thousand times.
#[derive(Clone, Debug, Default)]
pub struct Builder {
    /// Which pocket of a layer each cell has been walked as, for [`mouths`].
    /// One byte a cell, and the only buffer here that is not a reading of the
    /// geometry.
    marks: Vec<u8>,
    inside: [Vec<u8>; 3],
    distance: [Vec<u16>; 3],
    stack: Vec<u32>,
    /// Runs of a pocket still to have their neighbours above and below
    /// looked at, as row, first column and last.
    spans: Vec<[u32; 3]>,
    rough: Vec<f32>,
    smooth: Vec<f32>,
    /// Rows of the window that carry a rise at all. Most of one does not —
    /// measured on a 925-layer duct, 84.5% of the cells come out at zero —
    /// and a row of zeroes blurs to a row of zeroes, so [`Builder::blur`]
    /// walks the live rows and leaves the rest as they were resized.
    live: Vec<bool>,
    /// Threads a pass over a window is split between, asked of the machine
    /// once rather than once a layer.
    lanes: usize,
    /// True once a window over budget has been reported, so a part that is
    /// too wide says so once rather than once a layer.
    warned: bool,
}

/// Which layer each of the three sets in a [`Builder`] describes.
const HERE: usize = 0;
const ABOVE: usize = 1;
const BELOW: usize = 2;

/// The layers one surface is worked out from, and the two lengths that say
/// how to read them.
#[derive(Clone, Copy, Debug)]
pub struct Slice<'a> {
    pub here: &'a Cells,
    /// `None` at the top of an object, and `below` is `None` at its first
    /// layer; a file that prints its objects one at a time has several of
    /// each, and comparing across that boundary would measure one object's
    /// layer against another's.
    pub above: Option<&'a Cells>,
    pub below: Option<&'a Cells>,
    /// The widest strip to follow, in mm. A strip is one layer of staircase
    /// seen from above, so its width is the layer height over the tangent of
    /// the surface's slope: at 0.2 mm layers, 1 mm of strip is an 11° slope
    /// and 4 mm is 3°. Everything up to it is followed at full amplitude, so
    /// that one layer's ramp meets the next one's exactly; past it the
    /// amplitude tapers away over a further [`FADE`] of the reach.
    pub reach: f64,
    /// Half the width of the bead whose path drew the outlines, in mm.
    ///
    /// What is traced is a **centreline**, and the model's own outline runs
    /// half a bead outside it — on both layers alike, so the strip comes out
    /// the right width and only the position across it is shifted. Left out,
    /// a bead of the visible wall reads as sitting on the outline itself and
    /// is taken the whole half layer down, when the surface where its
    /// centreline runs is a fifth of a millimetre up the ramp. On a 0.6 mm
    /// strip that is a third of a layer of error, and the finer the grid the
    /// more of it shows: at 0.3 mm cells the grid's own overshoot cancelled
    /// most of it by accident.
    pub bead: f64,
}

impl Builder {
    /// Works out where the surface of `slice.here` sits, given what is printed
    /// over it and what it stands on.
    pub fn build(&mut self, field: &mut Field, slice: Slice<'_>) {
        let Slice {
            here,
            above,
            below,
            reach,
            bead,
        } = slice;
        field.clear();
        // The middle of a vertical face: nothing of this layer is left
        // exposed, because the one above covers all of it, and nothing under
        // it slopes away, because the one below ends where it does. Both
        // halves of the answer are already known, and most of a prismatic part
        // is this. Measured on a 2000-layer square column, taking the full
        // path for it cost 480 s of the test suite.
        if above == Some(here) && below == Some(here) {
            return;
        }
        let grid = here.grid();
        let Some(window) = window(grid, [Some(here), above, below]) else {
            return;
        };
        if !(reach.is_finite() && reach > 0.0) {
            return;
        }
        let [left, bottom, right, top] = window;
        let width = (right - left + 1) as usize;
        let height = (top - bottom + 1) as usize;
        if width.saturating_mul(height) > MAX_WINDOW {
            self.refuse(grid, width, height);
            return;
        }

        for (set, cells) in [(HERE, Some(here)), (ABOVE, above), (BELOW, below)] {
            self.mark(set, cells, left, bottom, width, height);
            self.enclose(set, width, height);
        }
        // An upward-facing surface around a hole is a pocket this layer
        // encloses and the layer above opens wider, and until the two are
        // told apart every one of them reads as covered. Both pairs of
        // layers, so a countersink's tread is measured against the bore it
        // stands on as well as against the one above it.
        let cell = grid.cell();
        // The widest strip carried at all. Everything up to the reach is
        // followed at full amplitude, because that is the only amplitude at
        // which one layer's ramp meets the next one's, and the quarter past
        // it is where the amplitude tapers away instead — see [`FADE`].
        let carried = reach * (1.0 + FADE);
        // An upward-facing surface around a hole is a pocket this layer
        // encloses and the layer above opens wider, and until the two are
        // told apart every one of them reads as covered. Both pairs of
        // layers, so a countersink's tread is measured against the bore it
        // stands on as well as against the one above it.
        {
            // A hole is at least a bead across and its tread is at most the
            // widest strip followed, both stated in cells. Where the file
            // named no bead width the grid answers instead: [`MOUTH_SLACK`]
            // is already the narrowest feature it can express.
            let waist = cells_of(bead * 2.0, cell).max(MOUTH_SLACK);
            let tread = cells_of(carried, cell);
            let [own, over, under] = &mut self.inside;
            let (own, over, under) = (&mut own[..], &mut over[..], &mut under[..]);
            let (marks, stack, spans) = (&mut self.marks, &mut self.stack, &mut self.spans);
            mouths(own, over, marks, stack, spans, width, height, waist, tread);
            mouths(under, own, marks, stack, spans, width, height, waist, tread);
        }
        // Distance from a point of the strip to the outside of its own layer,
        // to the layer printed over it, and to the outside of the layer it
        // stands on. The first two put the point across the strip; the third
        // is what says the strip has a ramp under it rather than a wall.
        //
        // The three read one window each and write another, and no two of
        // them touch the same buffer, so they are taken a thread apiece. It
        // is the largest single cost in the transform — measured on a
        // 925-layer duct, 15.0 s of a 27 s run — and the only part of a layer
        // that splits without a seam between the pieces.
        {
            let Self {
                inside, distance, ..
            } = self;
            let [mine, over, under] = &*inside;
            let [to_mine, to_over, to_under] = distance;
            std::thread::scope(|scope| {
                scope.spawn(|| transform(mine, to_mine, false, width, height));
                scope.spawn(|| transform(over, to_over, true, width, height));
                transform(under, to_under, false, width, height);
            });
        }

        field.grid = grid;
        field.left = left;
        field.bottom = bottom;
        field.width = width;
        field.height = height;
        // Grown rather than emptied and refilled: a `fill` over the window
        // is one memset where `clear` and `resize` is a loop the compiler
        // does not always see through.
        let cells = width * height;
        grow(&mut field.rise, cells, 0);
        grow(&mut field.open, cells, false);
        grow(&mut self.rough, cells, 0.0);
        grow(&mut self.live, height, false);
        field.rise[..cells].fill(0);
        self.rough[..cells].fill(0.0);
        self.live[..height].fill(false);

        let mm = |value: u16| match value {
            UNREACHED => f64::INFINITY,
            value => f64::from(value) / SPAN * cell,
        };
        // What was traced is a path of bead centres, and a slicer puts the
        // visible wall's centre half a bead inside the outline it is cutting
        // to. So the model's own outline is half a bead further out than every
        // distance measured here, and the surface crosses the strip from
        // there.
        //
        // It cancels in the width of the strip, which is a difference of two
        // outlines shifted alike, and in the test for a slope, which is
        // another. It does not cancel in the place across the strip, and
        // leaving it out puts the visible wall half a bead too far along its
        // own climb: on a tread exactly one bead wide the wall is where it
        // should already be, and without this it would be taken down a whole
        // half layer onto the one below.
        let shift = bead.max(0.0);
        // The chamfer distance a strip of `carried` works out to, so the two
        // legs that make one up can be compared as integers and most of the
        // window answered before any of the arithmetic below. A strip at or
        // past `carried` puts `fading` at zero, which leaves the cell exactly
        // where the slicer left it; the extra unit is a whole fifth of a cell
        // of slack against the rounding in `mm`, so nothing that would have
        // moved is refused here.
        let span = (carried * SPAN / cell).ceil() + 1.0;
        let span = if span.is_finite() && span > 0.0 {
            span.min(f64::from(u32::MAX)) as u32
        } else {
            u32::MAX
        };
        // Every cell of the window answers for itself here, so the pass is
        // split into bands of whole rows, one to a thread.
        let step = self.band(width, height);
        {
            let Self {
                inside,
                distance,
                rough,
                live,
                ..
            } = self;
            let [to_mine, to_over, to_under] = &*distance;
            let (to_mine, to_over, to_under) = (&to_mine[..], &to_over[..], &to_under[..]);
            let [mine, over, _] = &*inside;
            let (mine, over) = (&mine[..], &over[..]);
            std::thread::scope(|scope| {
                let mut base = 0;
                for ((open, rough), live) in field.open[..cells]
                    .chunks_mut(step)
                    .zip(rough[..cells].chunks_mut(step))
                    .zip(live[..height].chunks_mut(step / width))
                {
                    let from = base;
                    base += open.len();
                    scope.spawn(move || {
                        for (offset, open) in open.iter_mut().enumerate() {
                            let at = from + offset;
                            *open = is_inside(mine[at]) && !is_inside(over[at]);
                            let up = to_over[at];
                            let out = to_mine[at];
                            // Either leg unreached, or the two of them
                            // already spanning the whole strip, and the cell
                            // keeps the height the slicer gave it.
                            if u32::from(up) + u32::from(out) >= span
                                || up == UNREACHED
                                || out == UNREACHED
                            {
                                continue;
                            }
                            let up = mm(up);
                            let out = mm(out);
                            let strip = up + out;
                            if strip <= 0.0 || !strip.is_finite() {
                                continue;
                            }
                            let down = mm(to_under[at]);
                            // Under a uniform slope the layer below reaches
                            // one strip further out than this one; under a
                            // vertical face it stops in the same place. A
                            // layer below that runs past the window is
                            // shallower still, so it counts as fully sloped.
                            // The gauge is only half a strip because the two
                            // figures are read off the same grid to different
                            // precisions — see [`SLOPE_MARGIN`].
                            let sloped = match down.is_finite() {
                                true => ((down - out) / (strip * SLOPE_MARGIN)).clamp(0.0, 1.0),
                                false => 1.0,
                            };
                            let fading = ((carried - strip) / (reach * FADE)).clamp(0.0, 1.0);
                            let across = ((out + shift) / strip).clamp(0.0, 1.0);
                            // Filled in for every cell of the window, covered
                            // ones included: reading the field between cells
                            // is what turns a strip into a ramp, and a covered
                            // cell is where a strip's high end continues.
                            // Fading on the width of the strip rather than on
                            // the distance to the layer above is what keeps
                            // that honest: a covered cell deep inside the part
                            // measures a strip as wide as the part, so it
                            // fades to nothing instead of leaking a rise into
                            // the strip beside it, while one just inside the
                            // strip measures the strip itself and continues it
                            // exactly.
                            rough[offset] = ((across - 0.5) * sloped * fading) as f32;
                        }
                        // Read back while the band is still in cache, so the
                        // blur can walk the rows that hold something.
                        for (row, live) in live.iter_mut().enumerate() {
                            let row = &rough[row * width..(row + 1) * width];
                            *live = row.iter().any(|rise| *rise != 0.0);
                        }
                    });
                }
            });
        }

        let flat = self.blur(field, width, height);
        field.flat = flat;
    }

    /// Says so, once, when a layer will not fit the budget.
    ///
    /// The grid is chosen for the span the survey measured, so this only
    /// fires where a layer's real outlines reach past it. Left silent it is
    /// indistinguishable from a file whose region markers were not
    /// recognised — `--verbose` reports no followed moves either way, and
    /// that is the reading README.md teaches — so the user is told which of
    /// the two it is. A warning and not a failure: the print this is
    /// post-processing may already be waiting on it.
    fn refuse(&mut self, grid: Grid, width: usize, height: usize) {
        if std::mem::replace(&mut self.warned, true) {
            return;
        }
        eprintln!(
            "corbel: warning: a layer spanning {:.0}x{:.0} mm needs {} grid cells of {:.3} mm, \
             over the {MAX_WINDOW} this transform keeps, so its surface is left as sliced",
            width as f64 * grid.cell(),
            height as f64 * grid.cell(),
            width.saturating_mul(height),
            grid.cell(),
        );
    }

    /// Evens out the rise over a cell either way.
    ///
    /// A distance measured on the grid is quantised to it, so the strip's
    /// width read at one point of a curve differs from the width read at the
    /// next by up to a cell, and the rise wobbles with it. The wobble is
    /// noise: it puts a ripple in a wall that should be a smooth ramp, and it
    /// breaks one long move into a dozen short ones because the height keeps
    /// leaving the tolerance a move is written to.
    ///
    /// A box blur is exactly the right tool here because the thing being
    /// smoothed is a **straight ramp**, and a symmetric average leaves a
    /// straight ramp exactly where it was. Only the noise moves.
    ///
    /// Measured on a 180 mm dome: without it the same 16532 surface moves came
    /// out as 54105 moves, and with it as 26610. A finer distance transform is
    /// not the answer — going from a 3-4 chamfer to 5-7-11 moved that figure
    /// by 1%, because what wobbles is the 0.3 mm grid the distance is measured
    /// on, not the direction it is measured in.
    fn blur(&mut self, field: &mut Field, width: usize, height: usize) -> bool {
        let Self {
            rough,
            smooth,
            live,
            ..
        } = self;
        grow(smooth, width * height, 0.0);
        smooth[..width * height].fill(0.0);
        // Along each row, then down each column: two passes of three taps
        // rather than one of nine. A row of zeroes stays a row of zeroes
        // through both, so only the rows that hold a rise are walked, and
        // for the pass down the columns that means the row itself or one of
        // the two either side of it.
        for (row, live) in live[..height].iter().enumerate() {
            if !live {
                continue;
            }
            for column in 0..width {
                let at = row * width + column;
                let left = rough[at - usize::from(column > 0)];
                let right = rough[at + usize::from(column + 1 < width)];
                smooth[at] = (left + rough[at] + right) / 3.0;
            }
        }
        // The pass down the columns writes the answer straight out at the
        // resolution it is kept in, rather than over a third buffer for a
        // pass of its own to quantise: measured on a 925-layer duct, that
        // pass alone was 1.9 s of a 27 s run. Neither pass is worth a thread
        // — they are a third of the cost of the one above them and the
        // threads cost more to start than the work saves.
        let mut flat = true;
        for row in 0..height {
            let (under, over) = (
                width * usize::from(row > 0),
                width * usize::from(row + 1 < height),
            );
            let touched =
                live[row] || (row > 0 && live[row - 1]) || (row + 1 < height && live[row + 1]);
            if !touched {
                continue;
            }
            for column in 0..width {
                let at = row * width + column;
                let mean = (smooth[at - under] + smooth[at] + smooth[at + over]) / 3.0;
                let steps = (f64::from(mean) * STEPS)
                    .round()
                    .clamp(-STEPS / 2.0, STEPS / 2.0);
                field.rise[at] = steps as i8;
                flat &= !field.open[at] || steps == 0.0;
            }
        }
        flat
    }

    /// Cells a thread takes of a window, always whole rows.
    ///
    /// A pass over every cell of a layer costs what the widest band costs, so
    /// the split is over what the machine really has. Below the floor a
    /// window is left to one thread: starting one costs tens of microseconds
    /// and a small part's layer is over in less than that.
    fn band(&mut self, width: usize, height: usize) -> usize {
        const LEAST: usize = 1 << 16;
        if self.lanes == 0 {
            self.lanes = std::thread::available_parallelism().map_or(1, |lanes| lanes.get());
        }
        let cells = width * height;
        let lanes = self.lanes.min(cells / LEAST).max(1);
        height.div_ceil(lanes) * width
    }

    /// Paints one layer's cells into the window, as the material of that
    /// layer with everything else left hollow for [`Builder::enclose`] to
    /// sort into inside and out.
    fn mark(
        &mut self,
        set: usize,
        cells: Option<&Cells>,
        left: i32,
        bottom: i32,
        width: usize,
        height: usize,
    ) {
        let inside = &mut self.inside[set];
        inside.clear();
        inside.resize(width * height, HOLLOW);
        let Some(cells) = cells else {
            return;
        };
        for (column, row) in cells.iter() {
            let (column, row) = (column - left, row - bottom);
            if column < 0 || row < 0 {
                continue;
            }
            let (column, row) = (column as usize, row as usize);
            if column < width && row < height {
                inside[row * width + column] = MATERIAL;
            }
        }
    }

    /// Fills in what the painted cells enclose.
    ///
    /// Sparse infill leaves most of a layer's interior empty, and the strip
    /// this module measures runs at the edge of a layer, so "is there material
    /// in this cell" is the wrong question — "is this cell inside the outline"
    /// is the right one. A flood from the border answers it: the perimeters
    /// are a closed ring of cells, so nothing outside can reach in past them,
    /// and everything the flood does not reach is inside. Stepping only along
    /// the axes is what keeps it from leaking through a ring diagonally.
    ///
    /// What the flood does not reach is recorded as [`MATERIAL`] or
    /// [`HOLLOW`] rather than merely as inside, because a bore's interior and
    /// a hollow layer's interior are the same thing to a flood and are not
    /// the same thing to a surface. [`mouths`] is what separates them.
    fn enclose(&mut self, set: usize, width: usize, height: usize) {
        let Self { inside, stack, .. } = self;
        let inside = &mut inside[set];
        stack.clear();

        let open = |at: usize, inside: &mut Vec<u8>, stack: &mut Vec<u32>| {
            if inside[at] == HOLLOW {
                inside[at] = OUTSIDE;
                stack.push(at as u32);
            }
        };
        for column in 0..width {
            open(column, inside, stack);
            open((height - 1) * width + column, inside, stack);
        }
        for row in 0..height {
            open(row * width, inside, stack);
            open(row * width + width - 1, inside, stack);
        }

        while let Some(at) = stack.pop() {
            let at = at as usize;
            let (column, row) = (at % width, at / width);
            if column > 0 {
                open(at - 1, inside, stack);
            }
            if column + 1 < width {
                open(at + 1, inside, stack);
            }
            if row > 0 {
                open(at - width, inside, stack);
            }
            if row + 1 < height {
                open(at + width, inside, stack);
            }
        }
    }
}

/// The chamfer transform of one set, as two raster passes over the window.
fn transform(inside: &[u8], distance: &mut Vec<u16>, within: bool, width: usize, height: usize) {
    distance.clear();
    distance.resize(width * height, UNREACHED);
    for (at, &held) in inside.iter().enumerate() {
        if is_inside(held) == within {
            distance[at] = 0;
        }
    }

    for row in 0..height {
        for column in 0..width {
            let at = row * width + column;
            if distance[at] == 0 {
                continue;
            }
            let mut best = distance[at];
            let mut step = |dx: isize, dy: isize, cost: u16| {
                let (x, y) = (column as isize + dx, row as isize + dy);
                if x < 0 || y < 0 || x >= width as isize || y >= height as isize {
                    return;
                }
                let neighbour = distance[y as usize * width + x as usize];
                best = best.min(neighbour.saturating_add(cost));
            };
            for (dx, dy, cost) in FORWARD {
                step(dx, dy, cost);
            }
            distance[at] = best;
        }
    }
    for row in (0..height).rev() {
        for column in (0..width).rev() {
            let at = row * width + column;
            if distance[at] == 0 {
                continue;
            }
            let mut best = distance[at];
            let mut step = |dx: isize, dy: isize, cost: u16| {
                let (x, y) = (column as isize + dx, row as isize + dy);
                if x < 0 || y < 0 || x >= width as isize || y >= height as isize {
                    return;
                }
                let neighbour = distance[y as usize * width + x as usize];
                best = best.min(neighbour.saturating_add(cost));
            };
            for (dx, dy, cost) in FORWARD {
                step(-dx, -dy, cost);
            }
            distance[at] = best;
        }
    }
}

/// The half of a 5-7-11 kernel a forward raster pass may read: every offset
/// that has already been visited. The backward pass reads the mirror of it.
const FORWARD: [(isize, isize, u16); 8] = [
    (-1, 0, NEAR),
    (0, -1, NEAR),
    (-1, -1, FAR),
    (1, -1, FAR),
    (-2, -1, LEAP),
    (2, -1, LEAP),
    (-1, -2, LEAP),
    (1, -2, LEAP),
];

/// Reads the pockets of `lower` that `upper` opens wider as air rather than
/// as something printed over.
///
/// A flood from the border cannot tell a bore from the interior of a part:
/// both are what the outline encloses, and neither is reachable from outside.
/// Calling both of them covered is deliberate and mostly right — a hollow
/// layer's interior is filled by infill laid at the same plane, so nothing
/// there is exposed to the air — but it swallows every upward-facing surface
/// around a hole. A countersink, a chamfered bore mouth or a funnel has a
/// **larger** hole on the layer above, so the tread between the two reads as
/// covered and is left as a step, and a countersunk screw hole shows its
/// staircase exactly as plainly as an outer slope does.
///
/// What separates them is whether the layer above encloses the same pocket or
/// opens it out. Three things have to hold at once, and each one refuses a
/// case the others let through:
///
/// - the pocket has to be hollow in `upper` **all the way across**,
/// - every cell **bordering** the pocket has to be hollow in `upper` too, so
///   that the pocket sits strictly inside a pocket of `upper` rather than
///   merely overlapping one, and
/// - `upper` has to carry it at least [`MOUTH_SLACK`] cells further.
///
/// The first is what makes this safe on sparse infill — which matters more
/// than the bug, because a rise written over infill is buried inside the part
/// where nothing can iron it flat. The gaps between one layer's infill lines
/// are crossed by the next layer's, so a pocket of `lower` is never wholly
/// hollow in `upper` unless `upper` really did print nothing over it. Where
/// the two layers' infill is identical, as a 2D honeycomb's is, the pocket is
/// hollow in `upper` and is not opened by a single cell, so the third refuses
/// it.
///
/// The second is what keeps a part's own interior out of this. A hole that
/// **narrows** upward leaves the layer above with one pocket where this layer
/// has two, so this layer's interior is contained in it and reads as opened
/// wider — but only by merging, and a merge shows at the border: the interior
/// is bounded by the outer wall as well, and the outer wall is printed on the
/// layer above too. Nesting is the property that means "opened out"; mere
/// containment is not. The same test refuses a hollow shell whose wall moves
/// inward, and accepts one whose wall moves outward, which is the right
/// answer for both.
///
/// Both members of a matched pair are carved, so the distance transforms read
/// the bore's own wall as the edge of the layer: the tread's low end is then
/// this layer's outline and its high end the layer above's, which is the same
/// climb as on any other slope, only facing inward.
///
/// `waist` and `tread` are what keep the three tests above honest on a real
/// slice, and both are the module's own arithmetic rather than a tolerance.
/// A pocket narrower than `waist` — one whole bead, in cells — is the gap the
/// rasterised centrelines left between two neighbouring beads and not a hole
/// at all, and a carry running further than `tread` — the widest strip the
/// transform follows, in cells — is a pocket sitting loose in a void rather
/// than a lip over a hole. Without them, measured on a 672-layer part sliced
/// at 15% gyroid with 0.42 mm beads: **2878 pockets accepted, half of them a
/// single cell across, carrying a median of 1301 rings** — 190 mm of claimed
/// tread — so 25.6 million cells of the layers above were carved out of
/// 66 thousand cells of pocket, the interior of the part read as an open
/// surface, and the walls buried inside it were followed and then printed
/// over. That is what `--zaa` re-metering a file by **+14.34%** looks like,
/// against the −0.71% the same file gives once a mouth has to be one.
#[allow(clippy::too_many_arguments)]
fn mouths(
    lower: &mut [u8],
    upper: &mut [u8],
    marks: &mut Vec<u8>,
    stack: &mut Vec<u32>,
    spans: &mut Vec<[u32; 3]>,
    width: usize,
    height: usize,
    waist: usize,
    tread: usize,
) {
    // Four marks, not one. A pocket already walked is remembered for good, so
    // no pocket is walked twice; the marks the walk through `upper` leaves are
    // put back as they were once each pocket is finished. Keeping the two
    // apart is what lets that walk cross a pocket of `lower` that was itself
    // refused — and it has to, because the tread of a countersink is part of
    // the part's own interior, which is refused before the bore is ever
    // reached.
    const FRESH: u8 = 0;
    const WALKED: u8 = 1;
    const POCKET: u8 = 2;
    const CARRIED: u8 = 3;
    const RECARRIED: u8 = 4;

    let cells = width * height;
    if marks.len() < cells {
        marks.resize(cells, FRESH);
    }
    marks[..cells].fill(FRESH);
    // Nothing hollow can sit on the window's edge — [`window`] keeps
    // [`MARGIN`] cells clear around every layer and [`Builder::enclose`]
    // floods that ring from outside — so every cell walked below is a whole
    // row and column in from it and its four neighbours are simply an index
    // away. Stated here rather than assumed, because the walk visits a cell
    // for every hollow cell of a layer and the two divisions that working a
    // column out costs were most of this function's time: measured on a
    // 925-layer duct, 10.1 s of a 35 s run.
    for column in 0..width {
        for at in [column, (height - 1) * width + column] {
            lower[at] = OUTSIDE;
            upper[at] = OUTSIDE;
        }
    }
    for row in 0..height {
        for at in [row * width, row * width + width - 1] {
            lower[at] = OUTSIDE;
            upper[at] = OUTSIDE;
        }
    }
    let around = |at: usize| [at - 1, at + 1, at - width, at + width];

    for seed in 0..width * height {
        if marks[seed] != FRESH || !is_hollow(lower[seed]) {
            continue;
        }
        // One pocket of `lower`, walked once, along with the two things that
        // have to be true of the layer above it.
        //
        // A whole run of a row at a time rather than a cell at a time: three
        // quarters of a window is the inside of the part, so the pocket that
        // holds it is most of what this function ever walks, and taken cell
        // by cell it is walked as a wavefront that touches three arrays at
        // scattered addresses. By runs the same cells are read and written
        // along the rows they sit in. Which cells end up in `stack`, and in
        // what state, is unchanged; only the order they arrive in is, and
        // nothing downstream reads that — the rings below take a whole
        // frontier before counting one.
        stack.clear();
        spans.clear();
        let (mut held, mut surrounded) = (true, true);
        let mut across = 0usize;
        let (mut lowest, mut highest) = (usize::MAX, 0usize);
        fill(
            lower,
            upper,
            marks,
            stack,
            spans,
            width,
            seed,
            &mut held,
            &mut surrounded,
        );
        while let Some([row, from, to]) = spans.pop() {
            across = across.max((to - from + 1) as usize);
            lowest = lowest.min(row as usize);
            highest = highest.max(row as usize);
            let (from, to) = (from as usize, to as usize);
            for base in [(row as usize - 1) * width, (row as usize + 1) * width] {
                let mut x = from;
                while x <= to {
                    let at = base + x;
                    if !is_hollow(lower[at]) {
                        surrounded &= is_hollow(upper[at]);
                    } else if marks[at] != POCKET {
                        x = fill(
                            lower,
                            upper,
                            marks,
                            stack,
                            spans,
                            width,
                            at,
                            &mut held,
                            &mut surrounded,
                        );
                    }
                    x += 1;
                }
            }
        }
        let pocket = stack.len();
        // `fill` pushed a span before the walk began and every span is popped,
        // so the two always hold a real row.
        let rows = highest + 1 - lowest.min(highest);
        // A gap narrower than the bead that drew the outline is not a void.
        // What is painted here is a path of bead *centres*, so material
        // reaches half a bead either side of every cell marked, and the cells
        // left clear between two neighbouring beads are inside the plastic
        // rather than inside a hole. Measured on a 672-layer part sliced with
        // 0.42 mm beads: of the 2878 pockets this accepted, half were **one
        // cell** across and 97% were under three, every one of them the gap
        // between two lines of a solid region.
        let hole = across >= waist && rows >= waist;

        // How far `upper` carries the same pocket past this one, a ring of
        // cells at a time. The first ring is this layer's own wall.
        //
        // The walk is stopped, and the pocket refused with it, once it has run
        // further than the widest strip the transform follows: a tread that
        // wide is not a lip over a hole, it is the pocket sitting loose inside
        // a void of `upper`, and carving that void reads a strip where the
        // part has none. Nothing is given up by the bound — past `carried` a
        // strip's `fading` is zero, so no cell out there could move a bead
        // anyway. On the same file the pockets accepted ran a median of 1301
        // rings, 190 mm of "tread", into the sparse-infill interior.
        let mut rings = 0;
        let mut escaped = false;
        if held && surrounded && hole {
            let mut edge = pocket;
            let mut cursor = 0;
            while cursor < edge {
                while cursor < edge {
                    let at = stack[cursor] as usize;
                    cursor += 1;
                    for next in around(at) {
                        if !is_hollow(upper[next]) {
                            continue;
                        }
                        marks[next] = match marks[next] {
                            FRESH => CARRIED,
                            WALKED => RECARRIED,
                            _ => continue,
                        };
                        stack.push(next as u32);
                    }
                }
                rings += usize::from(stack.len() > edge);
                edge = stack.len();
                if rings > tread {
                    escaped = true;
                    break;
                }
            }
        }

        if rings >= MOUTH_SLACK && !escaped {
            for &at in &stack[..pocket] {
                lower[at as usize] = MOUTH;
            }
            for &at in stack.iter() {
                let at = at as usize;
                if upper[at] == HOLLOW {
                    upper[at] = MOUTH;
                }
            }
        }
        for &at in &stack[..pocket] {
            marks[at as usize] = WALKED;
        }
        for &at in &stack[pocket..] {
            let at = at as usize;
            marks[at] = match marks[at] {
                RECARRIED => WALKED,
                _ => FRESH,
            };
        }
    }
}

/// Marks the whole run of cells hollow in `lower` that holds `at`, records it
/// for its neighbours above and below to be scanned, and returns the column
/// it ends at.
///
/// The two cells that stopped the run are neighbours of the pocket and are
/// answered here; the ones above and below it are answered by whoever pops
/// the run back off `spans`.
#[allow(clippy::too_many_arguments)]
fn fill(
    lower: &[u8],
    upper: &[u8],
    marks: &mut [u8],
    stack: &mut Vec<u32>,
    spans: &mut Vec<[u32; 3]>,
    width: usize,
    at: usize,
    held: &mut bool,
    surrounded: &mut bool,
) -> usize {
    const POCKET: u8 = 2;
    let row = at / width;
    let base = row * width;
    let (mut from, mut to) = (at, at);
    while is_hollow(lower[from - 1]) {
        from -= 1;
    }
    while is_hollow(lower[to + 1]) {
        to += 1;
    }
    *surrounded &= is_hollow(upper[from - 1]) && is_hollow(upper[to + 1]);
    for at in from..=to {
        marks[at] = POCKET;
        stack.push(at as u32);
        *held &= is_hollow(upper[at]);
    }
    spans.push([row as u32, (from - base) as u32, (to - base) as u32]);
    to - base
}

/// Makes room for `cells` values without shrinking, so a window that comes
/// and goes costs one allocation rather than one a layer.
fn grow<T: Clone>(buffer: &mut Vec<T>, cells: usize, value: T) {
    if buffer.len() < cells {
        buffer.resize(cells, value);
    }
}

/// A length in millimetres as a whole number of cells, rounded up, and never
/// wider than a window may be. A length the file never stated arrives here as
/// zero or as something not finite, and answers zero, which every caller
/// reads as "the grid decides".
fn cells_of(length: f64, cell: f64) -> usize {
    let cells = (length / cell).ceil();
    match cells.is_finite() && cells > 0.0 {
        true => (cells as usize).min(MAX_WINDOW),
        false => 0,
    }
}

/// The box covering every cell of every layer given, with a clear ring around
/// it.
fn window(grid: Grid, layers: [Option<&Cells>; 3]) -> Option<[i32; 4]> {
    let mut box_: Option<[i32; 4]> = None;
    for cells in layers.into_iter().flatten() {
        if cells.grid() != grid {
            return None;
        }
        let Some(bounds) = cells.bounds() else {
            continue;
        };
        box_ = Some(box_.map_or(bounds, |had| {
            [
                had[0].min(bounds[0]),
                had[1].min(bounds[1]),
                had[2].max(bounds[2]),
                had[3].max(bounds[3]),
            ]
        }));
    }
    let [left, bottom, right, top] = box_?;
    Some([
        left.saturating_sub(MARGIN),
        bottom.saturating_sub(MARGIN),
        right.saturating_add(MARGIN),
        top.saturating_add(MARGIN),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A filled square of side `size` centred on the origin, drawn as a raster
    /// so its inside is genuinely enclosed.
    fn square(size: f64) -> Cells {
        let mut cells = Cells::default();
        let half = size / 2.0;
        cells.draw((-half, -half), (half, -half), None);
        cells.draw((half, -half), (half, half), None);
        cells.draw((half, half), (-half, half), None);
        cells.draw((-half, half), (-half, -half), None);
        cells.settle();
        cells
    }

    /// A square plate of side `outer` with a square hole of side `hole`
    /// through the middle of it, both drawn as rings so the flood fill sees
    /// the hole as a pocket of its own.
    fn plate(outer: f64, hole: f64) -> Cells {
        let mut cells = square(outer);
        let half = hole / 2.0;
        cells.draw((-half, -half), (half, -half), None);
        cells.draw((half, -half), (half, half), None);
        cells.draw((half, half), (-half, half), None);
        cells.draw((-half, half), (-half, -half), None);
        cells.settle();
        cells
    }

    fn field_of(here: &Cells, above: Option<&Cells>, below: Option<&Cells>, reach: f64) -> Field {
        // Fixtures draw the outline itself rather than a bead's centreline,
        // so there is no half bead to add back.
        field_with(here, above, below, reach, 0.0)
    }

    fn field_with(
        here: &Cells,
        above: Option<&Cells>,
        below: Option<&Cells>,
        reach: f64,
        bead: f64,
    ) -> Field {
        let mut builder = Builder::default();
        let mut field = Field::default();
        builder.build(
            &mut field,
            Slice {
                here,
                above,
                below,
                reach,
                bead,
            },
        );
        field
    }

    /// The rise across the right-hand side of a set of nested squares, from
    /// `from` mm out to `to` mm out, sampled every 0.1 mm.
    fn profile(field: &Field, from: f64, to: f64) -> Vec<f64> {
        let steps = ((to - from) / 0.1).round() as usize;
        (0..=steps)
            .map(|step| field.at(from + step as f64 * 0.1, 0.0))
            .collect()
    }

    /// The case the whole module exists for: a shallow cone, whose outlines
    /// shrink by the same step every layer. The strip between two of them is
    /// the tread of one stair, and the surface crosses it in a straight line
    /// from half a layer under the plane to half a layer over it.
    #[test]
    fn a_uniform_slope_is_followed_from_one_plane_to_the_next() {
        // Each layer is 12 mm narrower a side than the one below, so the strip
        // between 4.0 mm and 10.0 mm out is 6 mm wide all the way round.
        let (here, above, below) = (square(20.0), square(8.0), square(32.0));
        let field = field_of(&here, Some(&above), Some(&below), 12.0);
        assert!(!field.is_flat());

        let across = profile(&field, 4.5, 9.5);
        let (low, high) = (across[across.len() - 1], across[0]);
        assert!(low < -0.3, "the outer edge sits low: {across:?}");
        assert!(high > 0.3, "the inner edge sits high: {across:?}");
        // Monotonic across the strip: no step anywhere in it.
        for pair in across.windows(2) {
            assert!(pair[1] <= pair[0] + 1e-9, "{pair:?} rises going out");
        }
        // And straight, not merely monotonic. A cell is 0.3 mm and the values
        // are quantised, so the line is fitted with a little room.
        let span = across.len() - 1;
        let worst = across
            .iter()
            .enumerate()
            .map(|(step, rise)| {
                let along = step as f64 / span as f64;
                (rise - (high + (low - high) * along)).abs()
            })
            .fold(0.0, f64::max);
        assert!(worst < 0.04, "the climb is straight: {worst} off");
    }

    /// A slope so shallow the grid can barely tell one layer's outline from
    /// the next is still followed for the whole of its climb.
    ///
    /// The tread below is a **difference** of two grid distances and can come
    /// out at zero; the strip is a **sum** of two and cannot come out under a
    /// cell. Comparing them one for one reads a uniform slope as a partial
    /// one — measured on a 60 mm spherical cap, a mean of 0.368 where the
    /// geometry says 1.0 — so the gauge is half a strip. Here the step is one
    /// cell a side, which is the worst the grid can express.
    #[test]
    fn a_slope_finer_than_the_grid_is_still_followed_the_whole_way() {
        let cell = Grid::default().cell();
        let step = cell * 2.0;
        let (here, above, below) = (
            square(20.0),
            square(20.0 - step * 2.0),
            square(20.0 + step * 2.0),
        );
        let field = field_of(&here, Some(&above), Some(&below), 12.0);
        assert!(!field.is_flat());

        // A gauge that compared the tread with the strip one for one would
        // halve this: the tread reads one cell where the strip reads two.
        let across = profile(&field, 10.0 - step - cell, 10.0 + cell);
        let high = across.iter().copied().fold(f64::MIN, f64::max);
        let low = across.iter().copied().fold(f64::MAX, f64::min);
        assert!(high - low > 0.6, "the whole climb: {across:?}");
    }

    /// What a slicer traces is a path of bead centres, and it puts the visible
    /// wall's centre half a bead inside the outline it is cutting to. So the
    /// model's outline is half a bead further out than anything measured here,
    /// and where a bead sits across the strip has to be read from there.
    ///
    /// The case that shows it: a tread exactly one bead wide, where the wall's
    /// centre lands on the model surface's own mid-height and the slicer has
    /// already put it where it belongs. Without the half bead the same wall
    /// reads as sitting on the outer edge of the strip, and would be taken a
    /// whole half layer down onto the one below.
    #[test]
    fn a_bead_is_placed_from_the_outline_and_not_from_its_own_centre() {
        let bead = Grid::default().cell() * 2.0;
        let tread = bead * 2.0;
        let (here, above, below) = (
            square(20.0),
            square(20.0 - tread * 2.0),
            square(20.0 + tread * 2.0),
        );
        let (at_x, at_y) = (10.0 - Grid::default().cell() / 2.0, 0.0);

        let honest = field_with(&here, Some(&above), Some(&below), 12.0, bead).at(at_x, at_y);
        let naive = field_with(&here, Some(&above), Some(&below), 12.0, 0.0).at(at_x, at_y);
        assert!(naive < 0.0, "read from its own centre it drops: {naive}");
        assert!(honest > 0.0, "read from the outline it does not: {honest}");
        // Half a bead of a tread of two, less what the blur mixes in from the
        // cell beside it, where the climb has already reached the next plane
        // and stops.
        assert!(honest - naive > 0.35, "{honest} against {naive}");
    }

    /// Nothing is printed over a flat top, so there is no far edge to climb
    /// to and no way to tell where in the layer the surface really is. Lower
    /// the whole face by half a layer and it would be metered for half the gap
    /// as well, which starves a surface that was correct as sliced.
    #[test]
    fn a_flat_top_with_nothing_above_it_is_left_alone() {
        let here = square(20.0);
        let below = square(20.0);
        let field = field_of(&here, None, Some(&below), 12.0);
        assert!(field.is_flat());
        assert_eq!(field.at(9.0, 0.0), 0.0);
        assert!(field.is_open(9.0, 0.0), "exposed, and still left alone");
    }

    /// A ledge with a wall standing on it has a strip like a slope's, and it
    /// is not one: the model's surface stops dead at the ledge's edge instead
    /// of carrying on down. The layer below is what tells them apart, and
    /// under a vertical face it ends in the same place this one does.
    #[test]
    fn a_ledge_under_a_vertical_wall_is_not_mistaken_for_a_slope() {
        let here = square(20.0);
        let above = square(8.0);
        // The wall below runs straight down, so the layer under this one
        // reaches no further out than it does.
        let straight = field_of(&here, Some(&above), Some(&square(20.0)), 12.0);
        assert!(straight.is_flat(), "a vertical face leaves the ledge flat");

        // The same geometry with the wall sloping away below is followed.
        let sloped = field_of(&here, Some(&above), Some(&square(32.0)), 12.0);
        assert!(!sloped.is_flat());
    }

    /// The shortcut for the middle of a vertical face has to answer exactly
    /// what measuring it would have. A column one cell wider above is the
    /// same geometry as far as this transform is concerned and takes the long
    /// way round, so the two agreeing is the shortcut being sound.
    #[test]
    fn the_middle_of_a_vertical_face_is_answered_without_measuring_it() {
        let here = square(20.0);
        let column = field_of(&here, Some(&square(20.0)), Some(&square(20.0)), 12.0);
        assert!(column.is_flat());
        assert!(
            !column.is_open(9.0, 0.0),
            "the layer above covers all of it"
        );
        assert!(!column.is_open(0.0, 0.0));

        // Wide enough above that the two differ by a cell, narrow enough that
        // what it leaves exposed is nothing a bead could be moved across.
        let measured = field_of(
            &here,
            Some(&square(20.0 + Grid::default().cell())),
            Some(&square(20.0)),
            12.0,
        );
        assert!(measured.is_flat(), "a vertical face is flat either way");
    }

    /// A strip wider than the reach is a surface shallower than the tool was
    /// asked to follow, and it has to fade out rather than stop dead. Where
    /// the fade runs is the whole of the question: inside the range being
    /// followed it leaves a riser at every layer boundary instead of one at
    /// the end, so it runs past it — full amplitude up to the reach, tapering
    /// away over a further [`FADE`] of it.
    #[test]
    fn a_strip_wider_than_the_reach_fades_out_instead_of_ending_in_a_step() {
        let (here, above, below) = (square(20.0), square(8.0), square(32.0));
        // The strip is 6 mm of geometry and 6.3 mm of grid, so the three
        // reaches put it inside the reach, half way through the fade past it,
        // and past the fade entirely.
        let inside = field_of(&here, Some(&above), Some(&below), 12.0);
        let fading = field_of(&here, Some(&above), Some(&below), 5.2);
        let beyond = field_of(&here, Some(&above), Some(&below), 4.0);

        let at = |field: &Field| field.at(9.5, 0.0).abs();
        assert!(at(&inside) > 0.3, "well inside the reach: {}", at(&inside));
        assert!(at(&fading) > 0.0, "still followed: {}", at(&fading));
        assert!(
            at(&fading) < at(&inside) * 0.8,
            "fading out: {} against {}",
            at(&fading),
            at(&inside)
        );
        assert!(beyond.is_flat(), "past the fade entirely");
    }

    /// The reason the fade runs past the reach rather than inside it. Two
    /// layers of one slope meet at full amplitude and at no other: layer k's
    /// surface ends half a layer over its own plane exactly where layer k+1's
    /// begins half a layer under its, one layer higher. Scale both by `f` and
    /// what is left between them is a riser of `(1 - f)` layers — at every
    /// boundary in the band, not one at the end of it.
    ///
    /// The three reaches here put the same strip at 1.00, 0.87 and 0.75 of the
    /// reach, which at 0.2 mm layers is a surface at 1.00°, 1.15° and 1.33°.
    /// With the fade running inward over the last quarter of the reach those
    /// left 1.000, 0.479 and 0.008 of a layer standing at the boundary; the
    /// first two are the whole staircase this transform exists to remove, at
    /// slopes it was counting as followed.
    #[test]
    fn two_layers_of_one_slope_meet_without_a_riser_between_them() {
        // The strip is 6 mm of geometry and 21 cells of grid, and it is the
        // grid the fade is applied to.
        let strip = Grid::default().cell() * 21.0;
        for share in [1.00, 0.87, 0.75] {
            let reach = strip / share;
            // One layer of a cone that loses 12 mm a side per layer, and the
            // layer above it. Both treads are 6 mm wide, and they meet at
            // 10 mm out: the lower one's high edge and the upper one's low.
            let lower = field_of(
                &square(32.0),
                Some(&square(20.0)),
                Some(&square(44.0)),
                reach,
            );
            let upper = field_of(
                &square(20.0),
                Some(&square(8.0)),
                Some(&square(32.0)),
                reach,
            );
            assert!(
                !lower.is_flat() && !upper.is_flat(),
                "at a reach of {reach}"
            );

            // In layer heights: the upper layer's plane is one above the
            // lower's, and its surface has to carry on from where the lower
            // one's left off. The fixture's own offset is 0.07 of a layer,
            // being where the grid puts the outline inside its cell.
            let step = 1.0 + upper.at(10.0, 0.0) - lower.at(10.0, 0.0);
            assert!(
                step.abs() <= 0.2,
                "a reach of {reach} leaves {step} of a layer standing",
            );
        }
    }

    /// Sparse infill leaves a layer's interior mostly empty, so the strip has
    /// to be measured against what the outline encloses rather than against
    /// where plastic happens to sit. Otherwise every gap between two infill
    /// lines reads as the outside of the part — and the fixtures here draw
    /// nothing but a ring, so an enclosure that did not work would put the
    /// middle of the layer above out in the open.
    #[test]
    fn the_hollow_inside_of_a_layer_counts_as_covered() {
        let field = field_of(&square(20.0), Some(&square(8.0)), Some(&square(32.0)), 12.0);
        assert!(!field.is_open(0.0, 0.0), "under the layer above");
        assert!(!field.is_open(0.0, 3.0), "and so is the rest of it");
        assert!(field.is_open(9.5, 0.0), "the strip is exposed");
        assert!(!field.is_open(11.0, 0.0), "outside the layer entirely");
    }

    /// The mirror of the case above, and what the whole distinction is for: a
    /// hole that opens upward — a countersink, a chamfered bore mouth, a
    /// funnel — leaves a tread facing the sky with nothing printed over it.
    /// Read as covered it keeps its staircase, right where a screw head has
    /// to sit.
    #[test]
    fn a_hole_that_opens_upward_is_a_surface_and_is_followed() {
        // A plate whose bore widens by 2 mm a side per layer, so the tread
        // between one layer's bore and the next is 2 mm wide.
        let here = plate(20.0, 6.0);
        let above = plate(20.0, 10.0);
        let below = plate(20.0, 2.0);
        let field = field_of(&here, Some(&above), Some(&below), 12.0);
        assert!(!field.is_flat(), "the tread is a surface");

        assert!(field.is_open(4.0, 0.0), "nothing is printed over the tread");
        assert!(!field.is_open(0.0, 0.0), "and the bore itself is air");
        // The cone crosses the layer going out: at this layer's bore wall it
        // is at the bottom of the layer, at the next layer's at the top.
        let (low, high) = (field.at(3.4, 0.0), field.at(4.6, 0.0));
        assert!(low < -0.15, "the inner edge sits low: {low}");
        assert!(high > 0.15, "the outer edge sits high: {high}");
        for pair in profile(&field, 3.4, 4.6).windows(2) {
            assert!(pair[1] >= pair[0] - 1e-9, "{pair:?} falls going out");
        }
    }

    /// The other half of that guard, and the more important half: a hole that
    /// holds its width or narrows going up is not a surface, and neither is
    /// the interior of the part around it.
    ///
    /// Getting this wrong the other way would reshape sparse infill, which is
    /// buried where nothing can iron it flat. A narrowing hole is the case
    /// that makes mere containment insufficient: the layer above has one
    /// pocket where this one has two, so this layer's interior sits inside a
    /// pocket of it and still is not opened by it. What says so is the
    /// border — the interior is bounded by the outer wall as well, and the
    /// outer wall is printed on the layer above too.
    #[test]
    fn a_hole_that_holds_its_width_or_narrows_is_left_alone() {
        for above in [plate(20.0, 6.0), plate(20.0, 4.0)] {
            let here = plate(20.0, 6.0);
            // Sloping away below, so the layer is measured rather than
            // answered by the shortcut for the middle of a vertical face.
            let below = plate(32.0, 6.0);
            let field = field_of(&here, Some(&above), Some(&below), 12.0);

            assert!(!field.is_open(0.0, 0.0), "the bore is covered");
            assert!(!field.is_open(4.0, 0.0), "and so is the plate around it");
            // Nothing of the layer is left exposed, so nothing of it is
            // followed and every bead stays where the slicer put it.
            assert!(field.is_flat(), "the layer is left as sliced");
        }
    }

    /// Sparse infill and a hollow column are the same thing to a flood fill,
    /// and both have to stay covered. The layer above a hollow column is the
    /// same size as this one, so it opens nothing — and where a part is
    /// hollow because its infill is sparse, that infill crosses this layer's
    /// gaps in any case.
    #[test]
    fn a_hollow_column_is_still_covered_by_its_own_infill() {
        let here = square(20.0);
        let above = square(20.0);
        // Two cells wider below, so the vertical-face shortcut does not
        // answer this one before it is measured.
        let below = square(20.0 + Grid::default().cell() * 2.0);
        let field = field_of(&here, Some(&above), Some(&below), 12.0);

        assert!(field.is_flat(), "nothing of it is exposed");
        assert!(!field.is_open(0.0, 0.0), "the middle is covered");
        assert!(!field.is_open(9.5, 0.0), "and so is the wall");
    }

    /// A first layer has nothing under it to say which way the surface went,
    /// and the plate holds it flat in any case.
    #[test]
    fn a_layer_with_nothing_beneath_it_is_left_flat() {
        let field = field_of(&square(20.0), Some(&square(8.0)), None, 12.0);
        assert!(field.is_flat());
    }

    #[test]
    fn nothing_to_measure_is_not_an_allocation_or_a_panic() {
        let empty = Cells::default();
        assert!(field_of(&empty, None, None, 6.0).is_flat());
        assert!(field_of(&square(20.0), Some(&empty), Some(&empty), f64::NAN).is_flat());
        assert!(field_of(&square(20.0), Some(&empty), Some(&empty), 0.0).is_flat());
        assert!(field_of(&square(20.0), Some(&empty), Some(&empty), -1.0).is_flat());
        // Far outside any window, which must read as flat rather than index
        // into somebody else's cell.
        let field = field_of(&square(20.0), Some(&square(8.0)), Some(&square(32.0)), 12.0);
        assert_eq!(field.at(1e6, -1e6), 0.0);
        assert_eq!(field.at(f64::NAN, 0.0), 0.0);
        assert!(!field.is_open(f64::NAN, 0.0));
        assert!(!field.is_open(1e6, -1e6));
    }

    /// A coordinate no printer could reach must not turn into a window with a
    /// cell for every micron between here and there.
    #[test]
    fn a_window_no_printer_could_span_is_refused() {
        let mut wild = square(20.0);
        wild.draw((0.0, 0.0), (9000.0, 9000.0), None);
        wild.settle();
        assert!(field_of(&wild, Some(&square(8.0)), Some(&square(32.0)), 12.0).is_flat());
    }

    /// Most of a window is a strip too wide to carry any rise at all, and it
    /// is answered by comparing the two legs of the strip as they are
    /// measured — whole chamfer units — rather than by working each of them
    /// back into millimetres first. The two have to agree about where the
    /// boundary is, so this pins it from both sides: the same 6.3 mm of grid
    /// is followed under a reach whose fade just carries it and left alone
    /// under one that just does not.
    #[test]
    fn the_last_strip_the_fade_carries_is_still_followed() {
        let (here, above, below) = (square(20.0), square(8.0), square(32.0));
        let carried = field_of(&here, Some(&above), Some(&below), 5.1);
        let dropped = field_of(&here, Some(&above), Some(&below), 5.0);
        assert!(
            carried.at(9.5, 0.0).abs() > 0.0,
            "a strip inside the fade keeps its rise: {}",
            carried.at(9.5, 0.0)
        );
        assert!(dropped.is_flat(), "a strip past the fade is left as sliced");
    }

    /// A rise sits on a handful of the window's rows and the blur walks only
    /// those, so the rows either side of a live one have to be written all
    /// the same: the pass down the columns is what carries a rise off the row
    /// that holds it.
    #[test]
    fn a_rise_on_one_row_still_reaches_the_rows_either_side_of_it() {
        let (width, height) = (8, 7);
        let mut builder = Builder {
            rough: vec![0.0; width * height],
            live: vec![false; height],
            ..Builder::default()
        };
        builder.rough[3 * width + 4] = 1.0;
        builder.live[3] = true;
        let mut field = Field {
            width,
            height,
            rise: vec![0; width * height],
            open: vec![true; width * height],
            ..Field::default()
        };

        assert!(!builder.blur(&mut field, width, height), "something rose");
        assert_ne!(field.rise[2 * width + 4], 0, "the row above the live one");
        assert_ne!(field.rise[4 * width + 4], 0, "the row below the live one");
    }

    /// A pocket is filled a run of a row at a time, and the arms of a U meet
    /// only at its foot — so a row across it holds two runs of one pocket.
    /// What settles whether it is a mouth is that **every** cell of it is
    /// open above, so a walk that comes apart at the foot carves an arm that
    /// the layer above covers.
    #[test]
    fn a_pocket_a_row_cuts_in_two_is_still_one_pocket() {
        // `.` outside, `#` this layer's material, `o` what it encloses and
        // did not print in.
        let lower = [
            "..............",
            "..##########..",
            "..##########..",
            "..##o###o###..",
            "..##o###o###..",
            "..##o###o###..",
            "..##ooooo###..",
            "..##########..",
            "..##########..",
            "..##########..",
            "..............",
            "..............",
        ];
        let open = [
            "..............",
            "..oooooooooo..",
            "..oooooooooo..",
            "..oooooooooo..",
            "..oooooooooo..",
            "..oooooooooo..",
            "..oooooooooo..",
            "..oooooooooo..",
            "..oooooooooo..",
            "..oooooooooo..",
            "..............",
            "..............",
        ];
        let (width, height) = (14, 12);
        let read = |rows: &[&str; 12]| -> Vec<u8> {
            rows.iter()
                .flat_map(|row| row.bytes())
                .map(|cell| match cell {
                    b'#' => MATERIAL,
                    b'o' => HOLLOW,
                    _ => OUTSIDE,
                })
                .collect()
        };
        let carve = |upper: Vec<u8>| -> Vec<u8> {
            let (mut lower, mut upper) = (read(&lower), upper);
            mouths(
                &mut lower,
                &mut upper,
                &mut Vec::new(),
                &mut Vec::new(),
                &mut Vec::new(),
                width,
                height,
                MOUTH_SLACK,
                width.max(height),
            );
            lower
        };

        let carved = carve(read(&open));
        for row in 3..=5 {
            assert_eq!(carved[row * width + 4], MOUTH, "the near arm, row {row}");
            assert_eq!(carved[row * width + 8], MOUTH, "the far arm, row {row}");
        }
        for column in 4..=8 {
            assert_eq!(
                carved[6 * width + column],
                MOUTH,
                "the foot, column {column}"
            );
        }

        // One cell of the far arm printed over, which disqualifies the whole
        // pocket — the near arm included, because it is the same pocket.
        let mut covered = read(&open);
        covered[3 * width + 8] = MATERIAL;
        let carved = carve(covered);
        assert!(
            !carved.contains(&MOUTH),
            "a pocket the layer above covers anywhere is not a mouth"
        );
    }

    /// Reads an ASCII picture of a layer: `#` is material, `o` is what the
    /// outline encloses and nothing was printed in, `.` is outside.
    fn picture(rows: &[&str]) -> Vec<u8> {
        rows.iter()
            .flat_map(|row| row.bytes())
            .map(|cell| match cell {
                b'#' => MATERIAL,
                b'o' => HOLLOW,
                _ => OUTSIDE,
            })
            .collect()
    }

    /// What a run of [`mouths`] made of one layer and the layer above it.
    fn carved(lower: &[&str], upper: &[&str], waist: usize, tread: usize) -> (Vec<u8>, Vec<u8>) {
        let width = lower[0].len();
        let height = lower.len();
        let (mut lower, mut upper) = (picture(lower), picture(upper));
        mouths(
            &mut lower,
            &mut upper,
            &mut Vec::new(),
            &mut Vec::new(),
            &mut Vec::new(),
            width,
            height,
            waist,
            tread,
        );
        (lower, upper)
    }

    /// What is painted into the window is a path of bead **centres**, so the
    /// plastic reaches half a bead either side of every cell of it and the
    /// cells left clear between two neighbouring beads are inside the
    /// material rather than inside a hole. A flood fill cannot tell the two
    /// apart, and a speck the size of one cell is the commonest thing in a
    /// solid region: measured on a 672-layer part, of the 2878 pockets this
    /// accepted, **half were one cell across** and 97% were under three, and
    /// between them they carved 25.6 million cells of the layers above out of
    /// 66 thousand cells of pocket — every wall buried in the interior then
    /// read as an exposed surface, was followed, and was printed over.
    #[test]
    fn a_gap_between_two_beads_is_not_a_hole_that_opens_upward() {
        // Solid all the way across but for one cell the raster left clear.
        let speck = [
            "..............",
            "..##########..",
            "..##########..",
            "..####o#####..",
            "..##########..",
            "..##########..",
            "..##########..",
            "..##########..",
            "..............",
            "..............",
        ];
        // Two cells clear, which is a hole this grid can express.
        let hole = [
            "..............",
            "..##########..",
            "..##########..",
            "..####oo####..",
            "..####oo####..",
            "..##########..",
            "..##########..",
            "..##########..",
            "..............",
            "..............",
        ];
        // The layer above prints nothing over either of them, so both are
        // nested in a pocket of it and both are carried far past their own.
        let open = [
            "..............",
            "..oooooooooo..",
            "..oooooooooo..",
            "..oooooooooo..",
            "..oooooooooo..",
            "..oooooooooo..",
            "..oooooooooo..",
            "..oooooooooo..",
            "..............",
            "..............",
        ];
        let width = 14;

        let (lower, upper) = carved(&speck, &open, MOUTH_SLACK, width);
        assert_eq!(
            lower[3 * width + 6],
            HOLLOW,
            "one cell of clearance is not a bore"
        );
        assert!(
            !upper.contains(&MOUTH),
            "and it must not carve the layer above it"
        );

        // The same picture with the pocket one cell wider each way is a hole,
        // so this refuses a width and not a pocket.
        let (lower, upper) = carved(&hole, &open, MOUTH_SLACK, width);
        assert_eq!(lower[3 * width + 6], MOUTH, "a bore a bead across is one");
        assert!(upper.contains(&MOUTH), "and its tread is carved with it");
    }

    /// A mouth is a lip over a hole, so its tread is a slope like any other
    /// and cannot be wider than the widest strip this transform follows.
    /// Where the carry runs past that, what the pocket sits in is not a lip
    /// but a void — the sparse interior of the part — and carving it reads a
    /// strip where there is none. Nothing is given up by the bound: out past
    /// `carried` a strip's `fading` is zero, so no cell there could move a
    /// bead in any case. Measured on the same 672-layer part, the pockets
    /// accepted ran a **median of 1301 rings** of claimed tread, 190 mm of
    /// it, straight across the inside of the object.
    #[test]
    fn a_pocket_loose_in_a_void_is_not_a_lip_over_a_hole() {
        let bore = [
            "..............",
            "..##########..",
            "..##########..",
            "..###ooo####..",
            "..###ooo####..",
            "..###ooo####..",
            "..##########..",
            "..##########..",
            "..............",
            "..............",
        ];
        // A countersink: the same bore one ring wider, and walled beyond it.
        let lip = [
            "..............",
            "..##########..",
            "..##ooooo###..",
            "..##ooooo###..",
            "..##ooooo###..",
            "..##ooooo###..",
            "..##ooooo###..",
            "..##########..",
            "..............",
            "..............",
        ];
        // The same bore with nothing walling it at all.
        let void = [
            "..............",
            "..oooooooooo..",
            "..oooooooooo..",
            "..oooooooooo..",
            "..oooooooooo..",
            "..oooooooooo..",
            "..oooooooooo..",
            "..oooooooooo..",
            "..............",
            "..............",
        ];
        let width = 14;
        let tread = 3;

        let (lower, upper) = carved(&bore, &lip, MOUTH_SLACK, tread);
        assert_eq!(lower[4 * width + 6], MOUTH, "a tread the lip bounds");
        assert!(upper.contains(&MOUTH), "and the lip is carved with it");

        let (lower, upper) = carved(&bore, &void, MOUTH_SLACK, tread);
        assert_eq!(
            lower[4 * width + 6],
            HOLLOW,
            "a carry that runs past the reach is not a tread"
        );
        assert!(
            !upper.contains(&MOUTH),
            "so nothing of the layer above is carved either"
        );
    }

    /// The passes over a window are split into bands of whole rows, one to a
    /// thread, so a band boundary must not show in the answer.
    #[test]
    fn splitting_a_window_between_threads_leaves_the_same_surface() {
        // Wide enough that the split really happens: below the floor in
        // `Builder::band` a window is left to one thread whatever is asked.
        let (here, above, below) = (square(200.0), square(188.0), square(212.0));
        let slice = Slice {
            here: &here,
            above: Some(&above),
            below: Some(&below),
            reach: 12.0,
            bead: 0.0,
        };
        let mut alone = Field::default();
        let mut split = Field::default();
        let mut one = Builder {
            lanes: 1,
            ..Builder::default()
        };
        let mut many = Builder::default();
        one.build(&mut alone, slice);
        many.build(&mut split, slice);

        assert!(!alone.is_flat(), "the fixture has a surface to follow");
        assert!(
            many.band(split.width, split.height) < split.width * split.height,
            "the window really was split"
        );
        assert_eq!(alone.rise, split.rise);
        assert_eq!(alone.open, split.open);
        assert_eq!(alone.is_flat(), split.is_flat());
    }
}
