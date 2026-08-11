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
const STRAIGHT: f64 = 1e-4;

/// A miter longer than this many times the offset is a spike, and the vertex
/// falls back to a plain normal offset rather than being thrown out to a point
/// no bead was ever laid at.
const MITER_LIMIT: f64 = 4.0;

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
/// inside, which is every open fragment a thin wall broke into, and where an
/// arc cannot be moved without distorting the circle it was drawn on.
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
        let landed = corner(current, into, out_of, delta);

        // A vertex an arc starts from decides the radius that whole arc is
        // swept at, so it is pulled onto the circle the arc will be drawn on;
        // the miter is already within a few nanometres of it wherever the two
        // edges meet smoothly. A vertex only an arc arrives at is worth the
        // same treatment, since the straight move after it can start
        // anywhere.
        let circle = after
            .circle(current, delta)
            .or(before.circle(current, delta));
        moved.push(match circle {
            Some((centre, radius)) => project(landed, centre, radius).unwrap_or(landed),
            None => landed,
        });
    }

    keeps_its_arcs(&moved, edges).then_some(moved)
}

/// Whether every arc of an offset loop still runs round one circle.
///
/// Two arcs meeting at a corner want their shared vertex on two different
/// circles, so one of them ends off its own; past [`ARC_SLACK`] the loop is
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
    let start = (from.0 - centre.0, from.1 - centre.1);
    let end = (to.0 - centre.0, to.1 - centre.1);
    let radius = start.0.hypot(start.1);
    if radius <= 0.0 || radius.is_nan() {
        return chord;
    }
    let cross = start.0 * end.1 - start.1 * end.0;
    let dot = start.0 * end.0 + start.1 * end.1;
    let mut swept = cross.atan2(dot);
    if clockwise {
        swept = -swept;
    }
    if swept <= 0.0 {
        swept += std::f64::consts::TAU;
    }
    radius * swept
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
/// crossing and the vertex simply follows the shared normal.
fn corner(at: (f64, f64), into: (f64, f64), out_of: (f64, f64), delta: f64) -> (f64, f64) {
    let before = normal(into);
    let after = normal(out_of);
    let turn = into.0 * out_of.1 - into.1 * out_of.0;

    if turn.abs() < STRAIGHT {
        return (at.0 + before.0 * delta, at.1 + before.1 * delta);
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
            return at;
        }
        return (at.0 + mid_x / length * delta, at.1 + mid_y / length * delta);
    }
    landed
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
        // Three points on one line, closed back on itself: no corner anywhere.
        let line = [(0.0, 0.0), (5.0, 0.0), (10.0, 0.0)];
        let moved = offset(&line, &straight(3), 0.1).expect("three points");
        assert!(close(moved[1], (5.0, 0.1)), "{moved:?}");
    }

    #[test]
    fn a_hairpin_falls_back_instead_of_spiking() {
        // Out and straight back: the miter would run to infinity.
        let spike = [(0.0, 0.0), (10.0, 0.0), (0.0, 0.0001), (0.0, 5.0)];
        let moved = offset(&spike, &straight(4), 0.1).expect("a closed loop");
        for point in &moved {
            assert!(
                point.0.abs() < 20.0 && point.1.abs() < 20.0,
                "no vertex may be thrown clear of the part: {moved:?}"
            );
        }
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
        let points = [(10.0, 0.0), (0.0, 10.0), (0.0, 20.0), (10.0, 20.0)];
        let edges = [
            Edge::Arc {
                centre: (0.0, 0.0),
                clockwise: false,
            },
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
}
