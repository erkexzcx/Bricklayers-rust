#!/usr/bin/env python3
"""The surface model the Z anti-aliasing diagram is drawn from.

Mirrored from `src/zaa/surface.rs` and `src/zaa.rs`, function for function, so the
picture is the binary's own arithmetic rather than an artist's impression of
it:

    share     <- Builder::build's rise expression
    quantise  <- Field::rise, which stores a rise as one of STEPS steps
    rise      <- Pass::sample, which is `field.at() * height`, capped
    factor    <- Pass::build_plan, which meters a stretch for its own gap
    plane_of  <- the height a layer's beads are commanded at as sliced
    surface   <- the line those put every followed bead's top on

Two parts of the Rust are deliberately **not** mirrored, and both are exact for
the case the figure draws — a planar slope:

- the `footprint` grid the distances are measured on, and with it the half-bead
  shift `Slice::bead` applies. Both exist because the binary is handed bead
  **centrelines** quantised to a grid, and has to recover the model's outline
  from them. Here the distances are the model's outline already.
- `Builder::blur`, the 3x3 box blur over the rise field. A box blur is exact on
  a straight ramp, so it moves nothing the figure shows.

`pin.py` next to this file checks the constants against the Rust and puts a
synthetic slope through the compiled binary, so a change on either side that
drifts is caught rather than drawn.
"""

from __future__ import annotations

import math

FADE = 0.25
"""Share of the reach a strip fades out over **past** it, from
`src/zaa/surface.rs`.

A strip wider than the reach is a surface shallower than the tool was asked to
follow, and it has to fade rather than stop: the widest strip followed would
otherwise end in a step of exactly the size this exists to remove.

Where that fade runs is the whole of the question, and no constant carries it.
Two consecutive strips meet at full amplitude and at no other, so an amplitude
scaled by `f` leaves a riser of `(1 - f)` of a layer at every boundary it
touches: tapering *inside* the range being followed does not soften the last
step, it trades one step for a band of them. So everything out to the reach is
followed at full amplitude and the taper runs over the quarter past it, where
the surface is shallower than the tool claims to follow and was flat as sliced
anyway.
"""

STEPS = 200.0
"""Steps of a layer height a rise is stored in, from `src/zaa/surface.rs`.

The field holds one signed byte per cell, so the rise a diagram draws is
quantised exactly as the binary's is.
"""

SHALLOWEST_SLOPE = 1.0
"""Shallowest slope followed, in degrees, from `src/zaa.rs`.

How wide a strip may be is that slope's own tread, so it is derived per layer
rather than given: `height / tan`. There is no dial.
"""

SLOPE_MARGIN = 0.5
"""How much of a strip the layer below has to reach past this one, from
`src/zaa/surface.rs`.

A uniform slope reaches a whole strip past, so anything at or over half of one
reads as fully sloped. The figure draws a uniform slope, where this saturates
either way, so it changes nothing here — it is mirrored so that a change to it
is noticed rather than drawn over.
"""

CELL = 0.3
"""The coarsest footprint grid, in mm, from `src/geometry/footprint.rs`.

The binary picks a finer one per file, down to `Grid::FINEST`, from the span of
the part — resolution is bought with a fixed memory budget rather than fixed.
This is the ceiling, and the one the wall-stacking test always uses.
"""

STEP_OF_A_CELL = 0.5
"""How finely a move is sampled across a surface, as a share of a grid cell,
from `src/zaa.rs`.

The rise is measured on the grid and blurred over it, so half a cell samples
everything the field can express — whichever grid the file was given.
"""

STEP = CELL * STEP_OF_A_CELL
"""The same sampling step in mm, on the coarsest grid."""

WALL_CEILING = 0.0
"""The visible wall may be lowered onto the surface and never lifted off it.

`zaa::Pass::follow` holds it there. A bead of the outer wall standing proud is
out of reach of the nozzle's flat underside, so what would be ironed level is
free to bulge, and it does it on the face of the part.
"""


def clamp(value: float, low: float, high: float) -> float:
    return min(max(value, low), high)


def reach_for(height: float) -> float:
    """The widest strip followed on a layer of this height, in mm."""
    return height / math.tan(math.radians(SHALLOWEST_SLOPE))


def share(out: float, strip: float, down: float, reach: float) -> float:
    """Where the surface stands inside a layer, as a share of its height.

    `out` is the distance to the outside of this layer, `strip` the width of
    the band nothing is printed over, and `down` the distance to the outside of
    the layer below. Runs from -0.5 at the outer edge of a strip to +0.5 at the
    inner one, which is what makes consecutive strips meet exactly.
    """
    if not strip > 0.0:
        return 0.0
    # Under a uniform slope the layer below reaches one strip further out; under
    # a vertical face it stops in the same place and the ledge stays flat.
    sloped = (
        1.0
        if math.isinf(down)
        else clamp((down - out) / (strip * SLOPE_MARGIN), 0.0, 1.0)
    )
    # The widest strip carried at all. Everything up to the reach is followed
    # at full amplitude, because that is the only amplitude at which one
    # layer's ramp meets the next one's, and the quarter past it is where the
    # amplitude tapers away instead — see `FADE`.
    carried = reach * (1.0 + FADE)
    fading = clamp((carried - strip) / (reach * FADE), 0.0, 1.0)
    return (out / strip - 0.5) * sloped * fading


def quantise(value: float) -> float:
    """A share as the field stores it: one of `STEPS` steps, half either way."""
    steps = min(max(round(value * STEPS), -STEPS / 2.0), STEPS / 2.0)
    return steps / STEPS


def rise(
    out: float,
    strip: float,
    down: float,
    height: float,
    reach: float | None = None,
    ceiling: float = math.inf,
) -> float:
    """How far above its plane a bead is commanded, in mm."""
    if reach is None:
        reach = reach_for(height)
    return min(quantise(share(out, strip, down, reach)) * height, ceiling)


def factor(height: float, middle: float) -> float:
    """What a stretch's extrusion is multiplied by, for the gap it crosses.

    The layer under a surface is flat, so that gap is the layer height plus
    however far the stretch stands above the plane. `middle` is the mean rise
    over the stretch, which for one bead is its own.
    """
    if not height > 0.0:
        return 1.0
    return (height + middle) / height


def outline(layer: int, first: float, strip: float) -> float:
    """Where a layer's own outline runs, in mm along the slope.

    The outline is the path the slicer commands, which is the *centreline* of
    the visible wall rather than the face of the part. A slope rising in +x
    moves it one strip along per layer, and that strip is the tread of the
    staircase.
    """
    return first + layer * strip


def face_of(edge: float, skin_width: float) -> float:
    """Where a layer stops being inside itself, in mm.

    Half a visible bead outside the path that laid it — so the outermost wall's
    own centreline already stands a fifth of a millimetre into the strip, which
    is why the surface there is below the plane and the wall is lowered onto it
    rather than lifted.
    """
    return edge - skin_width / 2.0


def plane_of(layer: int, height: float) -> float:
    """The height a layer's beads are commanded at as sliced, in mm."""
    return (layer + 1) * height


def surface(x: float, face: float, strip: float, height: float) -> float:
    """Where the model's surface really is at `x`, in mm.

    `face` is where layer 0 stops being inside itself. A slicer takes its
    cross-section through the middle of a layer, so a layer's outline is where
    the surface passes half a layer below that layer's plane. Substituting that
    into `plane_of` and `share` cancels the layer index away: every strip is
    measured against **one straight line**, which is why consecutive strips
    join rather than step.
    """
    return height / 2.0 + height * (x - face) / strip


def centres(edge: float, spacing: float, count: int) -> list[float]:
    """Bead centres of one layer, laid inward from that layer's own outline.

    The visible wall is laid *on* the outline and the rest follow it inward, so
    every layer's lattice is anchored to its own edge and neighbouring layers
    are offset from each other by whatever the strip leaves over.
    """
    return [edge + at * spacing for at in range(count)]
