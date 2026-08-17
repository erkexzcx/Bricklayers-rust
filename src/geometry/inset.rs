//! Moving a closed loop sideways, toward the material behind it.
//!
//! A raised loop is displaced in Z, so the joint it makes with its neighbour is
//! a step rather than a flat face. Bringing the neighbour a few microns closer
//! squeezes the two beads together across that step, which closes the same
//! volume that extra flow would have filled — without adding material, and so
//! without growing the part.
//!
//! The offset is always to the **left** of the direction of travel. Slicers
//! emit an island's boundary anticlockwise and a hole's clockwise, so left is
//! the material side of both: left of an anticlockwise square points into it,
//! and left of a clockwise hole points out of the hole and into the wall around
//! it. Nothing here has to know which kind of loop it was handed.

/// Below this the two edges at a vertex are treated as one straight line, since
/// their intersection is too far away to be a corner. In mm of cross product
/// between two unit vectors, so it is the sine of the angle between them: one
/// ten-thousandth of a radian.
///
/// A sine is just as small at half a turn as it is at none, so this alone does
/// not say a vertex is straight — see [`corner`], which asks the dot product
/// which of the two it is.
const STRAIGHT: f64 = 1e-4;

/// A miter longer than this many times the offset is a spike, and the vertex
/// falls back to a plain normal offset rather than being thrown out to a point
/// no bead was ever laid at.
const MITER_LIMIT: f64 = 4.0;

/// How far an arc's swept angle may move from the one it was drawn through,
/// as a fraction of that angle, before the loop is left as the slicer wrote it.
///
/// How far an arc sweeps can only be read back off its two ends, and a miter
/// slides an end *along* the circle by `delta × tan(turn / 2)` as well as
/// across it — up to `MITER_LIMIT` times the offset before the fallback caps
/// it. An arc short enough for its two ends to change places therefore comes
/// back as the whole rest of the circle: a 0.02 mm arc mitered at both ends by
/// 0.1 mm is recovered as a near-complete turn, about 1500 times the sweep it
/// was drawn with, and the flow for that bead is metered from exactly this
/// number. A bead's material is proportional to its sweep, so this bound is
/// also the bound on how far that metering can be out. A quarter is far past
/// anything a smooth join asks for, since edges meeting tangentially slide
/// their shared end by `delta × tan(0)`.
const SWEEP_SLACK: f64 = 0.25;

/// How far apart an arc's two ends may sit on the circle it is drawn round,
/// once both have been moved, before the loop is left as the slicer wrote it.
///
/// An arc is commanded as a centre and a target, so a printer sweeps the
/// radius its start point sits at and steps to the target at the end. Where an
/// arc runs into another arc at a corner the two want their shared vertex on
/// different circles and one of them has to give, which leaves exactly that
/// step. Measured over 1788 arcs of two real slices, 90% of them land within
/// 1 µm of their own radius and the tail is a handful of sharp arc-to-arc
/// corners; one bead width of a tenth of a millimetre is far past any of them
/// and still under what the loop would suffer by not being moved at all.
const ARC_SLACK: f64 = 0.01;

/// One edge of a loop: the straight move a slicer usually emits, or the `G2`
/// or `G3` an arc-fitted one leaves in its place.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Edge {
    Straight,
    /// Absolute centre of the arc, and which way it turns. A `G2` is
    /// clockwise, which puts the material on the far side of the centre.
    Arc {
        centre: (f64, f64),
        clockwise: bool,
    },
}

impl Edge {
    /// Unit direction of travel at `at`, for an edge running `from` to `to`.
    ///
    /// An arc's is at right angles to its radius, which is what makes a
    /// corner between an arc and anything else the same calculation as a
    /// corner between two straight moves.
    fn tangent(&self, at: (f64, f64), from: (f64, f64), to: (f64, f64)) -> Option<(f64, f64)> {
        match self {
            Edge::Straight => direction(from, to),
            Edge::Arc {
                centre, clockwise, ..
            } => {
                let radial = direction(*centre, at)?;
                Some(match clockwise {
                    true => (radial.1, -radial.0),
                    false => (-radial.1, radial.0),
                })
            }
        }
    }

    /// The circle this edge is drawn on once it has been offset, as centre and
    /// radius, given a point it passes through. Left of travel is toward the
    /// centre for an anticlockwise arc and away from it for a clockwise one.
    fn circle(&self, at: (f64, f64), delta: f64) -> Option<((f64, f64), f64)> {
        let Edge::Arc {
            centre, clockwise, ..
        } = self
        else {
            return None;
        };
        let radius = (at.0 - centre.0).hypot(at.1 - centre.1);
        let moved = match clockwise {
            true => radius + delta,
            false => radius - delta,
        };
        (moved > 0.0).then_some((*centre, moved))
    }
}

/// Offsets a closed loop `delta` to the left of its direction of travel.
///
/// `points` are the loop's vertices in print order, without repeating the first
/// as the last, and `edges[k]` is how the loop travels from `points[k]` to the
/// point after it. Returns `None` where the loop is too short to have an
/// inside, which is every open fragment a thin wall broke into; where an arc
/// cannot be moved without distorting the circle it was drawn on, or without
/// changing how far round it the bead runs; where the loop turns back on
/// itself, which has no inside either; and where the miter would drag an edge
/// past its own far end. Moving such a loop correctly needs the whole polygon
/// clipped, and this module's contract is to move it correctly or leave it
/// alone — a loop left alone simply prints as sliced.
pub fn offset(points: &[(f64, f64)], edges: &[Edge], delta: f64) -> Option<Vec<(f64, f64)>> {
    if points.len() < 3 || edges.len() != points.len() || !delta.is_finite() {
        return None;
    }
    // An arc no wider than the offset would be turned inside out by it, and a
    // bead drawn at a negative radius goes round the far side of the centre.
    let drawable = points.iter().zip(edges).all(|(at, edge)| match edge {
        Edge::Straight => true,
        Edge::Arc { .. } => edge.circle(*at, delta).is_some(),
    });
    if !drawable {
        return None;
    }

    let count = points.len();
    let mut moved = Vec::with_capacity(count);
    for index in 0..count {
        let previous = points[(index + count - 1) % count];
        let current = points[index];
        let next = points[(index + 1) % count];
        let before = edges[(index + count - 1) % count];
        let after = edges[index];

        let (Some(into), Some(out_of)) = (
            before.tangent(current, previous, current),
            after.tangent(current, current, next),
        ) else {
            // A repeated point names no direction, so the vertex stays put
            // rather than being offset along an arbitrary normal.
            moved.push(current);
            continue;
        };

        // A vertex an arc starts from decides the radius that whole arc is
        // swept at, so it is pulled onto the circle the arc will be drawn on;
        // the miter is already within a few nanometres of it wherever the two
        // edges meet smoothly. A vertex only an arc arrives at is worth the
        // same treatment, since the straight move after it can start
        // anywhere.
        let circle = after
            .circle(current, delta)
            .or(before.circle(current, delta));
        // A cusp has no miter, but one with an arc on either side of it is
        // still settled by that arc's circle — a straight move meeting a bore
        // or a fillet at its tangent point doubles back exactly, and is the
        // ordinary way a slicer draws a rounded pocket. Only a cusp between
        // two straight edges leaves nowhere for the vertex to go.
        let landed = match corner(current, into, out_of, delta) {
            Some(landed) => landed,
            None if circle.is_some() => current,
            None => return None,
        };
        moved.push(match circle {
            Some((centre, radius)) => project(landed, centre, radius).unwrap_or(landed),
            None => landed,
        });
    }

    let intact = keeps_its_arcs(&moved, edges)
        && keeps_its_sweeps(points, &moved, edges)
        && keeps_its_order(points, &moved, edges);
    intact.then_some(moved)
}

/// Whether every arc of an offset loop still turns through the angle it was
/// drawn through, to within [`SWEEP_SLACK`].
///
/// A vertex is mitered along its two edges as well as across them, and an arc
/// states only a centre and a target, so the sweep the printer will run is
/// whatever the two moved ends describe. Slide them past each other and the
/// arc comes back as the rest of the circle, which is the length the bead is
/// then metered against.
fn keeps_its_sweeps(points: &[(f64, f64)], moved: &[(f64, f64)], edges: &[Edge]) -> bool {
    let count = points.len();
    edges.iter().enumerate().all(|(index, edge)| {
        let Edge::Arc { centre, clockwise } = edge else {
            return true;
        };
        let next = (index + 1) % count;
        let (Some(was), Some(now)) = (
            sweep(points[index], points[next], *centre, *clockwise),
            sweep(moved[index], moved[next], *centre, *clockwise),
        ) else {
            return false;
        };
        (now - was).abs() <= was * SWEEP_SLACK
    })
}

/// Whether every straight edge of an offset loop still runs the way it was
/// drawn.
///
/// A miter takes `delta × tan(turn / 2)` off the end of the edge it lands on,
/// so between two corners turning `a` and `b` the same way an edge shorter
/// than `delta × (tan(a/2) + tan(b/2))` comes out with its two ends the other
/// way round. The loop then crosses itself, and the bead laid backwards along
/// it is still metered as though it had a length. Two right angles need only
/// twice the offset, which at the widest `--extra-flow` is 0.16 mm — wider
/// than a chamfer or a gap fill. A corner turning the other way is not the
/// same case: the erosion of a reflex corner is genuinely sharp at the miter
/// point, and the edges either side of it grow rather than shrink.
fn keeps_its_order(points: &[(f64, f64)], moved: &[(f64, f64)], edges: &[Edge]) -> bool {
    let count = points.len();
    edges.iter().enumerate().all(|(index, edge)| {
        if !matches!(edge, Edge::Straight) {
            // An arc that turned itself inside out did so by losing its sweep,
            // which `keeps_its_sweeps` is what asks about.
            return true;
        }
        let next = (index + 1) % count;
        let Some(drawn) = direction(points[index], points[next]) else {
            // A repeated point named no direction to begin with, so the offset
            // cannot have reversed one.
            return true;
        };
        match direction(moved[index], moved[next]) {
            Some(now) => drawn.0 * now.0 + drawn.1 * now.1 > 0.0,
            None => false,
        }
    })
}

/// Whether every arc of an offset loop still runs round one circle.
///
/// Two arcs meeting at a corner want their shared vertex on two different
/// circles, so one of them ends off its own; past a hundredth of a millimetre
/// the loop is
/// better left where the slicer put it than drawn at a radius it was never
/// given. Public because a caller that adjusts a vertex afterwards — the one
/// closing a loop on its seam — has to ask again.
pub fn keeps_its_arcs(moved: &[(f64, f64)], edges: &[Edge]) -> bool {
    let count = moved.len();
    edges.iter().enumerate().all(|(index, edge)| {
        let Edge::Arc { centre, .. } = edge else {
            return true;
        };
        let span = |at: (f64, f64)| (at.0 - centre.0).hypot(at.1 - centre.1);
        let swept = span(moved[index]);
        swept > 0.0 && (span(moved[(index + 1) % count]) - swept).abs() <= ARC_SLACK
    })
}

/// `point` pulled onto the circle of `radius` about `centre`, along the radius
/// it already sits on.
fn project(point: (f64, f64), centre: (f64, f64), radius: f64) -> Option<(f64, f64)> {
    let out = direction(centre, point)?;
    Some((centre.0 + out.0 * radius, centre.1 + out.1 * radius))
}

/// How far the nozzle travels from `from` to `to` along `edge`, in mm. An arc
/// is followed round rather than cut across, so a bead's flow per mm survives
/// the loop being moved.
pub fn length(from: (f64, f64), to: (f64, f64), edge: Edge) -> f64 {
    let chord = (to.0 - from.0).hypot(to.1 - from.1);
    let Edge::Arc { centre, clockwise } = edge else {
        return chord;
    };
    let radius = (from.0 - centre.0).hypot(from.1 - centre.1);
    match sweep(from, to, centre, clockwise) {
        Some(turned) => radius * turned,
        None => chord,
    }
}

/// The angle an arc about `centre` turns through going `from` to `to`, in
/// radians and always positive: an arc that comes back to where it started has
/// swept the whole circle rather than none of it. `None` where an end sits on
/// the centre, which names no angle at all.
fn sweep(from: (f64, f64), to: (f64, f64), centre: (f64, f64), clockwise: bool) -> Option<f64> {
    let start = (from.0 - centre.0, from.1 - centre.1);
    let end = (to.0 - centre.0, to.1 - centre.1);
    let radius = start.0.hypot(start.1);
    let arrives = end.0.hypot(end.1);
    if radius <= 0.0 || radius.is_nan() || arrives <= 0.0 || arrives.is_nan() {
        return None;
    }
    let cross = start.0 * end.1 - start.1 * end.0;
    let dot = start.0 * end.0 + start.1 * end.1;
    let mut turned = cross.atan2(dot);
    if clockwise {
        turned = -turned;
    }
    if turned <= 0.0 {
        turned += std::f64::consts::TAU;
    }
    Some(turned)
}

/// The unit vector from `from` to `to`, or `None` where the two coincide.
fn direction(from: (f64, f64), to: (f64, f64)) -> Option<(f64, f64)> {
    let (dx, dy) = (to.0 - from.0, to.1 - from.1);
    let length = dx.hypot(dy);
    (length > 0.0 && length.is_finite()).then(|| (dx / length, dy / length))
}

/// Left of the direction of travel.
fn normal(direction: (f64, f64)) -> (f64, f64) {
    (-direction.1, direction.0)
}

/// Where a vertex lands once both of the edges meeting at it have been moved
/// `delta` to their left.
///
/// The two offset edges are extended until they cross, which is what keeps a
/// corner sharp instead of rounding it off. Where they run parallel there is no
/// crossing and the vertex simply follows the shared normal. `None` where the
/// vertex has no inside to be moved into, and so the whole loop is declined.
fn corner(at: (f64, f64), into: (f64, f64), out_of: (f64, f64), delta: f64) -> Option<(f64, f64)> {
    let before = normal(into);
    let after = normal(out_of);
    let turn = into.0 * out_of.1 - into.1 * out_of.0;
    let onward = into.0 * out_of.0 + into.1 * out_of.1;

    if turn.abs() < STRAIGHT {
        // The cross product is the sine of the turn, which is as near zero at
        // half a turn as it is at none. The dot product is what separates them,
        // and a loop that doubles back has no left-hand side: the two offset
        // edges lie on the same line displaced to opposite sides, so following
        // either normal puts the vertex a full 2 * delta clear of the other
        // edge and crosses the loop over itself.
        if onward < 0.0 {
            return None;
        }
        return Some((at.0 + before.0 * delta, at.1 + before.1 * delta));
    }

    // Both offset lines pass through the vertex displaced along their own
    // normal; solving for where they meet gives the mitered corner.
    let start = (at.0 + before.0 * delta, at.1 + before.1 * delta);
    let end = (at.0 + after.0 * delta, at.1 + after.1 * delta);
    let (gap_x, gap_y) = (end.0 - start.0, end.1 - start.1);
    let along = (gap_x * out_of.1 - gap_y * out_of.0) / turn;

    let landed = (start.0 + into.0 * along, start.1 + into.1 * along);
    let reach = (landed.0 - at.0).hypot(landed.1 - at.1);
    if reach > delta.abs() * MITER_LIMIT {
        // A hairpin throws the miter out to a spike far from any bead the
        // slicer laid, so the vertex keeps to the average of the two normals.
        let (mid_x, mid_y) = (before.0 + after.0, before.1 + after.1);
        let length = mid_x.hypot(mid_y);
        if length <= 0.0 {
            return None;
        }
        return Some((at.0 + mid_x / length * delta, at.1 + mid_y / length * delta));
    }
    Some(landed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn square_ccw() -> Vec<(f64, f64)> {
        vec![(0.0, 0.0), (10.0, 0.0), (10.0, 10.0), (0.0, 10.0)]
    }

    /// A loop a slicer emitted without arc fitting: every edge straight.
    fn straight(points: usize) -> Vec<Edge> {
        vec![Edge::Straight; points]
    }

    fn close(a: (f64, f64), b: (f64, f64)) -> bool {
        (a.0 - b.0).abs() < 1e-9 && (a.1 - b.1).abs() < 1e-9
    }

    /// An island is emitted anticlockwise, so left of travel is into it and the
    /// loop shrinks by the offset on every side.
    #[test]
    fn an_anticlockwise_loop_shrinks() {
        let moved = offset(&square_ccw(), &straight(4), 0.1).expect("a closed loop");
        let expected = [(0.1, 0.1), (9.9, 0.1), (9.9, 9.9), (0.1, 9.9)];
        for (got, want) in moved.iter().zip(expected) {
            assert!(close(*got, want), "{moved:?}");
        }
    }

    /// A hole is emitted clockwise, so the same rule moves its wall away from
    /// the hole and into the material around it: the hole opens up.
    #[test]
    fn a_clockwise_loop_grows() {
        let mut hole = square_ccw();
        hole.reverse();
        let moved = offset(&hole, &straight(4), 0.1).expect("a closed loop");
        let (left, bottom) = moved.iter().fold((f64::MAX, f64::MAX), |(x, y), point| {
            (x.min(point.0), y.min(point.1))
        });
        assert!(
            close((left, bottom), (-0.1, -0.1)),
            "a hole must open, not close: {moved:?}"
        );
    }

    /// The corner is mitered rather than rounded, so a 45 degree turn lands
    /// further out than the offset itself.
    #[test]
    fn a_corner_is_mitred_to_where_the_edges_cross() {
        let triangle = [(0.0, 0.0), (10.0, 0.0), (10.0, 10.0)];
        let moved = offset(&triangle, &straight(3), 0.1).expect("a closed loop");
        // The right angle at (10, 0) moves diagonally by delta on both axes.
        assert!(close(moved[1], (9.9, 0.1)), "{moved:?}");
        // The 45 degree corners reach further, as a miter must.
        let reach = (moved[0].0 - 0.0).hypot(moved[0].1 - 0.0);
        assert!(reach > 0.1, "a shallow corner must miter out: {moved:?}");
    }

    #[test]
    fn a_straight_run_offsets_along_its_own_normal() {
        // A mid-edge vertex of a real loop: no corner there, but the loop it
        // sits on encloses something. Three collinear points closed back on
        // themselves would instead be two reversals and no inside at all.
        let line = [
            (0.0, 0.0),
            (5.0, 0.0),
            (10.0, 0.0),
            (10.0, 10.0),
            (0.0, 10.0),
        ];
        let moved = offset(&line, &straight(5), 0.1).expect("a closed loop");
        assert!(close(moved[1], (5.0, 0.1)), "{moved:?}");
    }

    /// The miter at a sharp corner runs out to a spike far from any bead the
    /// slicer laid, so the vertex falls back to the average of the two normals
    /// and moves by exactly the offset. The turn here is 168.7 degrees, whose
    /// miter would reach ten times the offset, against the four
    /// [`MITER_LIMIT`] allows.
    #[test]
    fn a_hairpin_falls_back_instead_of_spiking() {
        let spike = [(0.0, 0.0), (10.0, 0.0), (0.0, 2.0), (-3.0, 5.0)];
        let moved = offset(&spike, &straight(4), 0.1).expect("a closed loop");
        let reach = (moved[1].0 - 10.0).hypot(moved[1].1);
        assert!(
            (reach - 0.1).abs() < 1e-9,
            "a hairpin moves by the offset, not by its miter: {reach} in {moved:?}"
        );
    }

    /// What this fixture has always exercised is the *straight* branch, not
    /// the miter limit: its turn is 1e-5, which is smaller than [`STRAIGHT`],
    /// and a cross product cannot tell a straight run from a complete
    /// reversal. Offsetting it along the incoming edge's normal put the vertex
    /// twice the offset clear of the outgoing edge, on the wrong side of it.
    #[test]
    fn a_near_reversal_is_declined_instead_of_jogged_across_its_own_edge() {
        let spike = [(0.0, 0.0), (10.0, 0.0), (0.0, 0.0001), (0.0, 5.0)];
        assert!(offset(&spike, &straight(4), 0.1).is_none());
    }

    /// A loop that doubles back on itself has no left-hand side where it
    /// turns: the two offset edges land on one line displaced to opposite
    /// sides, so either normal crosses the loop over itself. Nudged off half a
    /// turn it is an ordinary hairpin and does have one.
    #[test]
    fn a_complete_reversal_has_no_inside_and_is_declined() {
        let doubled_back = [(0.0, 0.0), (10.0, 0.0), (5.0, 0.0), (5.0, 5.0)];
        assert!(offset(&doubled_back, &straight(4), 0.1).is_none());

        let hairpin = [(0.0, 0.0), (10.0, 0.0), (5.0, 1.0), (5.0, 5.0)];
        assert!(offset(&hairpin, &straight(4), 0.1).is_some());
    }

    /// A gap fill is a long thin loop, so the edge across each of its ends is
    /// a short one between two right-angle corners, and a right angle takes
    /// the whole offset off the edge it turns onto.
    fn ribbon(width: f64) -> Vec<(f64, f64)> {
        vec![(0.0, 0.0), (10.0, 0.0), (10.0, width), (0.0, width)]
    }

    /// Twice the offset out of a 0.1 mm edge leaves it running backwards, and
    /// the bead laid along it is still metered as though it had a length. At
    /// the widest `--extra-flow` asks for the offset is 0.08 mm, so it does.
    #[test]
    fn a_segment_the_miter_turns_end_for_end_is_declined() {
        assert!(offset(&ribbon(0.1), &straight(4), 0.08).is_none());

        // The same loop at the offset the shipped default produces, where the
        // corners take 0.005 mm each and 0.09 mm of the edge is left.
        let moved = offset(&ribbon(0.1), &straight(4), 0.005).expect("a closed loop");
        assert!(close(moved[0], (0.005, 0.005)), "{moved:?}");
    }

    #[test]
    fn an_open_fragment_is_left_alone() {
        assert!(offset(&[(0.0, 0.0), (1.0, 0.0)], &straight(2), 0.1).is_none());
        assert!(offset(&[], &straight(0), 0.1).is_none());
        assert!(offset(&square_ccw(), &straight(4), f64::NAN).is_none());
        // An edge per vertex or the loop is not described at all.
        assert!(offset(&square_ccw(), &straight(3), 0.1).is_none());
    }

    /// A repeated point names no direction, and guessing one would swing the
    /// vertex somewhere the loop never went.
    #[test]
    fn a_repeated_point_stays_put() {
        let doubled = [(0.0, 0.0), (10.0, 0.0), (10.0, 0.0), (10.0, 10.0)];
        let moved = offset(&doubled, &straight(4), 0.1).expect("a closed loop");
        assert!(close(moved[1], (10.0, 0.0)), "{moved:?}");
    }

    /// A ring drawn as four quarter arcs about the origin, anticlockwise.
    fn ring(radius: f64, clockwise: bool) -> (Vec<(f64, f64)>, Vec<Edge>) {
        let mut points: Vec<(f64, f64)> = (0..4)
            .map(|step| {
                let angle = std::f64::consts::FRAC_PI_2 * step as f64;
                (radius * angle.cos(), radius * angle.sin())
            })
            .collect();
        if clockwise {
            points.reverse();
        }
        let edges = vec![
            Edge::Arc {
                centre: (0.0, 0.0),
                clockwise,
            };
            4
        ];
        (points, edges)
    }

    /// An arc keeps the centre it was drawn about; what moves is its radius,
    /// inward for an anticlockwise ring exactly as for a straight loop.
    #[test]
    fn an_anticlockwise_ring_of_arcs_shrinks_by_the_offset() {
        let (points, edges) = ring(10.0, false);
        let moved = offset(&points, &edges, 0.1).expect("a closed ring");
        for point in &moved {
            let radius = point.0.hypot(point.1);
            assert!((radius - 9.9).abs() < 1e-9, "{radius} in {moved:?}");
        }
    }

    /// A hole's wall is emitted clockwise, so left of travel is out of the
    /// hole and the arc's radius grows.
    #[test]
    fn a_clockwise_ring_of_arcs_grows_by_the_offset() {
        let (points, edges) = ring(10.0, true);
        let moved = offset(&points, &edges, 0.1).expect("a closed ring");
        for point in &moved {
            let radius = point.0.hypot(point.1);
            assert!((radius - 10.1).abs() < 1e-9, "{radius} in {moved:?}");
        }
    }

    /// Where an arc runs into a straight move, the vertex belongs to the arc:
    /// the radius the printer sweeps is read off the arc's own start point, so
    /// a vertex a few nanometres off it is drawn at the wrong radius all the
    /// way round, while the straight move can start anywhere.
    #[test]
    fn a_vertex_between_an_arc_and_a_line_lands_on_the_arc() {
        // A quarter arc about the origin, boxed in by three straight moves.
        // The straights arrive at and leave the arc across it rather than
        // along it, so neither end of the arc is a cusp.
        let points = [
            (10.0, 0.0),
            (0.0, 10.0),
            (0.0, 20.0),
            (20.0, 20.0),
            (20.0, 0.0),
        ];
        let edges = [
            Edge::Arc {
                centre: (0.0, 0.0),
                clockwise: false,
            },
            Edge::Straight,
            Edge::Straight,
            Edge::Straight,
            Edge::Straight,
        ];
        let moved = offset(&points, &edges, 0.1).expect("a closed loop");
        for at in [0, 1] {
            let radius = moved[at].0.hypot(moved[at].1);
            assert!((radius - 9.9).abs() < 1e-9, "{radius} at {at} in {moved:?}");
        }
    }

    /// An offset that would turn an arc inside out has no answer, and drawing
    /// it at a negative radius would send the bead round the far side.
    #[test]
    fn an_arc_narrower_than_the_offset_is_left_alone() {
        let (points, edges) = ring(0.05, false);
        assert!(offset(&points, &edges, 0.1).is_none());
    }

    /// The length of a bead is what its flow is metered against, and an arc's
    /// is round the circle rather than across the chord.
    #[test]
    fn an_arc_is_measured_round_rather_than_across() {
        let quarter = Edge::Arc {
            centre: (0.0, 0.0),
            clockwise: false,
        };
        let round = length((10.0, 0.0), (0.0, 10.0), quarter);
        assert!(
            (round - 10.0 * std::f64::consts::FRAC_PI_2).abs() < 1e-9,
            "{round}"
        );
        let across = length((10.0, 0.0), (0.0, 10.0), Edge::Straight);
        assert!((across - 200.0_f64.sqrt()).abs() < 1e-9, "{across}");
    }

    /// A clockwise quarter between the same two points is the other three
    /// quarters of the circle, so direction cannot be guessed from the ends.
    #[test]
    fn an_arc_is_measured_the_way_it_turns() {
        let long = length(
            (10.0, 0.0),
            (0.0, 10.0),
            Edge::Arc {
                centre: (0.0, 0.0),
                clockwise: true,
            },
        );
        let want = 10.0 * 3.0 * std::f64::consts::FRAC_PI_2;
        assert!((long - want).abs() < 1e-9, "{long}");
    }

    /// An arc of `swept` radians on a circle of 5 mm, with a straight move
    /// arriving at each of its ends across the circle rather than along it, so
    /// both ends are corners that miter and both miters slide the end along
    /// the circle. Anticlockwise, and every corner turns the same way, so the
    /// two slides eat into the sweep from opposite ends.
    fn nicked_ring(swept: f64) -> (Vec<(f64, f64)>, Vec<Edge>) {
        let points = vec![
            (5.0, 0.0),
            (5.0 * swept.cos(), 5.0 * swept.sin()),
            (-10.0, 10.0),
            (-10.0, -10.0),
        ];
        let edges = vec![
            Edge::Arc {
                centre: (0.0, 0.0),
                clockwise: false,
            },
            Edge::Straight,
            Edge::Straight,
            Edge::Straight,
        ];
        (points, edges)
    }

    /// The two miters slide this arc's ends 0.053 mm each way along a circle
    /// they were only 0.02 mm apart on, so they change places. Both are still
    /// exactly on the offset circle, which is all `keeps_its_arcs` asks, but
    /// the sweep the printer would run is now 6.265 radians against the 0.004
    /// it was drawn with: the bead is metered at 1535 times its own length.
    #[test]
    fn an_arc_whose_ends_the_miter_swaps_is_declined() {
        let (points, edges) = nicked_ring(0.004);
        assert!(offset(&points, &edges, 0.1).is_none());
    }

    /// The same corners against an arc long enough to survive them: the sweep
    /// comes back within 3.5% of the one it was drawn with, and the length the
    /// bead is metered against with it.
    #[test]
    fn an_arc_that_survives_the_miter_keeps_the_sweep_it_was_drawn_with() {
        let (points, edges) = nicked_ring(0.5);
        let moved = offset(&points, &edges, 0.1).expect("a closed loop");
        let centre = (0.0, 0.0);
        let was = sweep(points[0], points[1], centre, false).expect("an arc");
        let now = sweep(moved[0], moved[1], centre, false).expect("an arc");
        assert!(
            (now - was).abs() <= was * SWEEP_SLACK,
            "{now} against {was} in {moved:?}"
        );
        let ratio = length(moved[0], moved[1], edges[0]) / length(points[0], points[1], edges[0]);
        assert!((0.9..1.0).contains(&ratio), "{ratio} in {moved:?}");
    }

    /// A regular polygon, which is what a slicer emits wherever it has not
    /// fitted arcs.
    fn polygon(sides: usize, radius: f64, clockwise: bool) -> (Vec<(f64, f64)>, Vec<Edge>) {
        let mut points: Vec<(f64, f64)> = (0..sides)
            .map(|step| {
                let angle = std::f64::consts::TAU * step as f64 / sides as f64;
                (radius * angle.cos(), radius * angle.sin())
            })
            .collect();
        if clockwise {
            points.reverse();
        }
        let edges = straight(sides);
        (points, edges)
    }

    /// Straight sides joined tangentially by quarter arcs: every rounded box,
    /// every filleted pocket, and the shape arc fitting leaves behind.
    fn rounded_rectangle(half: (f64, f64), radius: f64) -> (Vec<(f64, f64)>, Vec<Edge>) {
        let (a, b, r) = (half.0, half.1, radius);
        let arc = |centre: (f64, f64)| Edge::Arc {
            centre,
            clockwise: false,
        };
        let points = vec![
            (-a + r, -b),
            (a - r, -b),
            (a, -b + r),
            (a, b - r),
            (a - r, b),
            (-a + r, b),
            (-a, b - r),
            (-a, -b + r),
        ];
        let edges = vec![
            Edge::Straight,
            arc((a - r, -b + r)),
            Edge::Straight,
            arc((a - r, b - r)),
            Edge::Straight,
            arc((-a + r, b - r)),
            Edge::Straight,
            arc((-a + r, -b + r)),
        ];
        (points, edges)
    }

    /// Declining costs the visible wall its move: it is then scaled without
    /// being moved, which grows the part by the offset. So the three guards
    /// have to leave alone everything a slicer routinely emits.
    ///
    /// Polygons from a coarse octagon to a 128-sided circle, rings of arcs and
    /// rounded rectangles, both windings, at the offset the shipped default
    /// produces (5 µm) and at the widest `--extra-flow` can ask for (80 µm):
    /// not one of the 60 offsets is declined, no edge loses more than 5% of
    /// its length against the 100% an inverted edge loses, and no arc's sweep
    /// moves by a millionth against the quarter [`SWEEP_SLACK`] allows.
    #[test]
    fn ordinary_geometry_is_never_declined() {
        let mut shapes = Vec::new();
        for clockwise in [false, true] {
            for sides in [8, 16, 36, 128] {
                for radius in [2.0, 5.0, 20.0] {
                    shapes.push(polygon(sides, radius, clockwise));
                }
            }
            shapes.push(ring(5.0, clockwise));
            shapes.push(rounded_rectangle((20.0, 10.0), 3.0));
            shapes.push(rounded_rectangle((4.0, 2.0), 0.5));
        }

        let mut shortest: f64 = f64::MAX;
        let mut drift: f64 = 0.0;
        for delta in [0.005, 0.08] {
            for (points, edges) in &shapes {
                let moved = offset(points, edges, delta)
                    .unwrap_or_else(|| panic!("{delta} declined {points:?}"));
                for (index, edge) in edges.iter().enumerate() {
                    let next = (index + 1) % points.len();
                    match edge {
                        Edge::Straight => {
                            let was = length(points[index], points[next], *edge);
                            let now = length(moved[index], moved[next], *edge);
                            shortest = shortest.min(now / was);
                        }
                        // An arc's length moves with its radius as well as its
                        // sweep, so it is the sweep alone that is compared.
                        Edge::Arc { centre, clockwise } => {
                            let was = sweep(points[index], points[next], *centre, *clockwise);
                            let now = sweep(moved[index], moved[next], *centre, *clockwise);
                            let (was, now) = (was.expect("an arc"), now.expect("an arc"));
                            drift = drift.max((now - was).abs() / was);
                        }
                    }
                }
            }
        }
        assert!(
            shortest > 0.95,
            "an edge lost {}%",
            (1.0 - shortest) * 100.0
        );
        assert!(drift < 1e-6, "an arc's sweep moved by {drift}");
    }
}
