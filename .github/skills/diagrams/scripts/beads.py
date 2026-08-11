#!/usr/bin/env python3
"""The bead model the diagrams are drawn from, mirrored from `src/brick.rs`.

Nothing here is a drawing convenience. Every function is the Python twin of a
Rust one, so a diagram cannot show a wall the binary would not produce:

    bead_spacing   <- brick::bead_spacing
    flow_ceiling   <- brick::flow_ceiling
    automatic_flow <- brick::automatic_flow
    rise           <- Pass::rise_at
    offsets        <- Pass::offset / Pass::rise_below, over a whole column
    skin_offset    <- Pass::skin_offset
    raised_loops   <- Pass::number_loops, for the simple monotonic case

`pin.py` next to this file checks them against the Rust, so a change on either
side that drifts is caught rather than drawn.
"""

from __future__ import annotations

import math

CORNER = 1.0 - math.pi / 4.0
"""Share of its own height a stadium bead loses to its two round caps."""

RAMP = 2
"""Layers a raised column takes to climb to its full offset."""

DEFAULT_EXTRA_FLOW = 0.05
MIN_EXTRA_FLOW = 0.0
MAX_EXTRA_FLOW = 0.5

REFERENCE_NOZZLE = 0.4
REFERENCE_HEIGHT = 0.2
REFERENCE_WIDTH = 0.45


def bead_spacing(height: float, width: float) -> float:
    """Centre-to-centre distance a slicer lays neighbouring beads at, in mm."""
    return width - height * CORNER


def flow_ceiling(height: float, spacing: float) -> float:
    """Flow at which a bead's edge reaches the centre of the loop beside it.

    Past this it is swallowing its neighbour rather than filling the corner
    between them. Derived, not chosen: ~1.89 on the reference profile.
    """
    return 2.0 - height * CORNER / spacing


def automatic_flow(
    height: float,
    width: float | None,
    extra: float = DEFAULT_EXTRA_FLOW,
) -> float:
    """Flow a wall is metered at, where a layer as thick as the nozzle takes
    `extra` over."""
    extra = min(max(extra, MIN_EXTRA_FLOW), MAX_EXTRA_FLOW)
    at_reference = extra * REFERENCE_HEIGHT / REFERENCE_NOZZLE
    if width is None or width <= 0.0 or height <= 0.0:
        return 1.0 + at_reference
    spacing = bead_spacing(height, width)
    if spacing <= 0.0:
        return 1.0 + at_reference
    reference = REFERENCE_HEIGHT / bead_spacing(REFERENCE_HEIGHT, REFERENCE_WIDTH)
    junction = (height / spacing) / reference
    return max(min(1.0 + at_reference * junction, flow_ceiling(height, spacing)), 1.0)


def rise(steps: int, height: float) -> float:
    """How far a raised loop stands above the plane after `steps` layers."""
    return height / 2.0 * min(steps, RAMP) / RAMP


def skin_offset(flow: float, skin_width: float, height: float) -> float:
    """How far the visible wall is brought in, in mm: half the width it gains.

    What it gains is `(flow - 1)` of its *spacing*, not of its nominal width,
    because the area scales at a fixed height and the round caps cost the same
    either way. Zero on the layer laid on the build plate, which `move_walls`
    declines — there is no staggered joint under it to close.
    """
    return (flow - 1.0) / 2.0 * bead_spacing(height, skin_width)


def raised_loops(loops: int) -> list[bool]:
    """Which loops of a wall are raised, numbered from the visible one.

    The visible wall is loop 0 and is never raised, so three loops leave both
    ends flat and raise the one between them, and four raise the far end.
    """
    return [phase % 2 == 1 for phase in range(loops)]


def offsets(layers: int, raised: bool, height: float, capped: bool) -> list[float]:
    """How far above the plane a column stands, layer by layer.

    Zero on the layer laid on the build plate, then the [`RAMP`] climb, then
    the steady half-layer, and zero again on a capped top where the wall ends
    and something is printed over it.
    """
    if not raised:
        return [0.0] * layers
    standing = [0.0] + [rise(step, height) for step in range(1, layers)]
    if capped and layers:
        standing[-1] = 0.0
    return standing


def spans(standing: list[float], height: float) -> list[tuple[float, float]]:
    """(bottom, top) of each bead of a column, in mm.

    A bead starts on top of whatever its column left on the layer below and
    ends at the nozzle, which is why a climbing bead is a quarter layer taller
    than its own layer and a capped one is half a layer shorter.
    """
    return [
        (layer * height + (standing[layer - 1] if layer else 0.0),
         (layer + 1) * height + standing[layer])
        for layer in range(len(standing))
    ]


def bead_width(flow: float, spacing: float, bead_height: float) -> float:
    """Width of a stadium bead `bead_height` tall, metered at `flow`, in mm.

    The slicer metered `height × spacing` of area per mm; multiplying E by
    `flow × span` deposits `span × flow × height × spacing` into a bead
    `span × height` tall. Solving `H(W − H(1 − π/4)) = flow × H × spacing`
    leaves `W = flow × spacing + H(1 − π/4)`, so the span goes entirely into
    the bead's height and the flow entirely into its width.
    """
    return flow * spacing + bead_height * CORNER
