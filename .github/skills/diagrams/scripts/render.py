#!/usr/bin/env python3
"""Draw a wall's cross-section as sliced and as bricked, side by side.

Every coordinate comes out of `beads.py`, which mirrors `src/brick.rs`, so the
picture is the binary's own arithmetic rather than an artist's impression of
it. Run `pin.py` first if you have touched either side.

    python3 render.py --output-dir .

writes `interlock-light.png` and `interlock-dark.png`, the pair README.md
switches between with `<picture>`.
"""
from __future__ import annotations

import argparse
import math
from dataclasses import dataclass
from pathlib import Path

import matplotlib

matplotlib.use("Agg")

import matplotlib.patheffects as effects  # noqa: E402
import matplotlib.pyplot as plt  # noqa: E402
from matplotlib.lines import Line2D  # noqa: E402
from matplotlib.patches import FancyBboxPatch, Polygon  # noqa: E402

import beads  # noqa: E402


@dataclass(frozen=True)
class Theme:
    """The two colourways GitHub picks between."""

    name: str
    paper: str
    ink: str
    faint: str
    highlight: str
    highlight_edge: str
    inner: str
    inner_edge: str
    lifted: str
    lifted_edge: str
    skin: str
    skin_edge: str
    seam: str


LIGHT = Theme(
    name="light",
    paper="#ffffff",
    ink="#1f2328",
    faint="#848d97",
    highlight="#f4f7fa",
    highlight_edge="#c8d1da",
    inner="#f5d9a4",
    inner_edge="#b07d2b",
    lifted="#e9a13b",
    lifted_edge="#8a5a12",
    skin="#8ec3f0",
    skin_edge="#2f6fb0",
    seam="#9e0f1c",
)

DARK = Theme(
    name="dark",
    paper="#0d1117",
    ink="#e6edf3",
    faint="#7d8590",
    highlight="#161c24",
    highlight_edge="#30363d",
    inner="#6b5220",
    inner_edge="#d9a441",
    lifted="#b3801f",
    lifted_edge="#f0c26b",
    skin="#2c5f96",
    skin_edge="#8ec3f0",
    seam="#ff5247",
)


def brick(
    left: float,
    right: float,
    bottom: float,
    top: float,
    radius: float,
    curve,
    outer: float | None = None,
    amp_left: float = 0.0,
    amp_right: float = 0.0,
    points: int = 12,
):
    """A bead's cross-section, drawn as a rectangle with softened corners.

    A bead is laid closer to its neighbour than it is wide, so drawing two of
    them at full width overlaps them into a blob. What each one actually owns
    is the ground between the midpoints to either side, which is what is drawn
    here — the beads tile, and the corner the flow exists to fill is the
    softening.

    `outer` softens the left pair on their own. The face of the part is free
    air: no flow presses that edge into a corner, so it keeps the profile the
    slicer gave it however much the joints behind it are fed.

    `amp_left` and `amp_right` bend a side into a wave. The boundary between
    two loops is a single curve that both of them are cut from — one column
    stands above the other, so where one bead's middle pushes out, the joint
    between two of its neighbour's beads takes it, and the roles swap at the
    next joint. They snap together: no overlap and no gap, which is what the
    flow does to a staggered seam. `curve` is what puts the crests and troughs
    in the right places; see `key_curve`.
    """
    limit = min((right - left) / 2.0, (top - bottom) / 2.0)
    rr = max(min(radius, limit), 0.0)
    rl = max(min(radius if outer is None else outer, limit), 0.0)

    def wave(x: float, amp: float, y: float) -> float:
        return x + amp * curve(y)

    def side(x: float, amp: float, low: float, high: float, steps: int = 40):
        return [
            (wave(x, amp, low + (high - low) * at / steps), low + (high - low) * at / steps)
            for at in range(steps + 1)
        ]

    def quarter(cx: float, cy: float, r: float, start: float, end: float):
        step = (end - start) / points
        return [
            (cx + r * math.cos(start + step * at), cy + r * math.sin(start + step * at))
            for at in range(points + 1)
        ]

    # Each arc is centred a radius in from the curve *where its own side
    # starts*, never from the value at the joint. Anchoring at the joint looks
    # right on a crest and steps by several microns on a slope, which is what
    # the two climbing layers sit on.
    half = math.pi / 2.0
    return [
        *quarter(wave(left, amp_left, bottom + rl) + rl, bottom + rl, rl, 2 * half, 3 * half),
        *quarter(wave(right, amp_right, bottom + rr) - rr, bottom + rr, rr, 3 * half, 4 * half),
        *side(right, amp_right, bottom + rr, top - rr),
        *quarter(wave(right, amp_right, top - rr) - rr, top - rr, rr, 0.0, half),
        *quarter(wave(left, amp_left, top - rl) + rl, top - rl, rl, half, 2 * half),
        *side(left, amp_left, top - rl, bottom + rl),
    ]


def phase_of(spans: list[tuple[float, float]]):
    """Where a height sits inside the raised column's own bead, 0 to 1.

    The key has to trough at that column's joints and crest at its middles.
    Taking those off its real beads rather than off a fixed period is what
    keeps the two climbing layers mating: their beads are a quarter layer
    taller than the rest, so a fixed period drifts out of step with them and
    the bricks tear.
    """

    def phase(y: float) -> float:
        for bottom, top in spans:
            if y < top and top > bottom:
                return max((y - bottom) / (top - bottom), 0.0)
        bottom, top = spans[-1]
        return 1.0 if top <= bottom else min((y - bottom) / (top - bottom), 1.0)

    return phase


def key_curve(spans: list[tuple[float, float]], height: float):
    """The shared boundary shape, from -1 at a joint to +1 at a bead's middle.

    Saturated rather than clipped, so the crest is a short flat run and every
    turn is smooth: the corner arcs at a joint then sit on a straight edge,
    which keeps the outline round and leaves the void three beads meeting at a
    junction actually have.
    """
    phase = phase_of(spans)

    def curve(y: float) -> float:
        turn = -math.cos(2.0 * math.pi * phase(y))
        return math.tanh(FLATTEN * turn) / math.tanh(FLATTEN)

    return curve


def wall(cfg, raised: list[bool]) -> list[dict]:
    """Every bead of the wall, as the ground it owns rather than as its width.

    A loop's own width still sets where it reaches: the outermost face of the
    visible wall is its real half-width out from its real centre, which is what
    keeps that face still while the flow widens the bead. Between two loops the
    boundary is the midpoint of the centres the *slicer* laid, so no two bricks
    overlap, no gap opens between them, and the flow shows up where it actually
    goes — in the joint — rather than as a wall sliding sideways.
    """
    spacing = beads.bead_spacing(cfg.height, cfg.width)
    skin_spacing = beads.bead_spacing(cfg.height, cfg.skin_width)
    flow = beads.automatic_flow(cfg.height, cfg.width, cfg.extra_flow)
    edges = [(loop + 0.5) * spacing for loop in range(cfg.loops - 1)]

    drawn = []
    columns = [
        beads.spans(beads.offsets(cfg.layers, lifts, cfg.height, cfg.capped), cfg.height)
        for lifts in raised
    ]

    for at in range(cfg.layers):
        # The plate presses its own beads, so that layer is metered as sliced
        # and the visible wall on it is not moved either.
        layer_flow = 1.0 if at == 0 else flow
        inward = 0.0 if at == 0 else beads.skin_offset(layer_flow, cfg.skin_width, cfg.height)

        for loop in range(cfg.loops):
            bottom, top = columns[loop][at]
            lane = skin_spacing if loop == 0 else spacing
            reach = beads.bead_width(layer_flow, lane, top - bottom) / 2.0
            drawn.append(
                {
                    "left": inward - reach if loop == 0 else edges[loop - 1],
                    # The innermost face is held where the slicer put it for the
                    # same reason the boundaries are: the flow goes into the
                    # joints, not into a thicker wall.
                    "right": (
                        (cfg.loops - 1) * spacing + beads.bead_width(1.0, spacing, cfg.height) / 2.0
                        if loop + 1 == cfg.loops
                        else edges[loop]
                    ),
                    "bottom": bottom,
                    "top": top,
                    "loop": loop,
                    "raised": raised[loop],
                    "flow": layer_flow,
                    "layer": at,
                }
            )
    return drawn


@dataclass(frozen=True)
class Wall:
    """The geometry one panel is drawn from."""

    height: float
    width: float
    skin_width: float
    loops: int
    layers: int
    extra_flow: float
    capped: bool


GAP = 0.28
"""Corner radius as a share of a layer's height, at the flow the slicer meant.

The void two beads leave between their rounded edges is the whole reason
`--extra-flow` exists, and at true scale it is a few microns across — invisible
beside a 0.2 mm layer. This is the one quantity in the picture drawn out of
scale, and it is drawn out of scale so that it can be seen at all.
"""

SWITCH_SIZE = 11.0
"""Point size of the "what --x does" line under the panel a transform produces.

Both figures name their own switch, in the same words at the same size and in
the same faint ink, so a reader moving between them reads one label rather than
two conventions. `contour.py` imports this.
"""

FLATTEN = 3.0
"""How hard the boundary wave saturates, so its crests are flats not points.

A crest has to be at least a corner radius long for the two bricks meeting
there to seat on a straight edge; below about 2.5 the arcs sit on a slope, the
outline kinks and the junction closes up when it should not. It saturates
rather than clipping, because a clip puts a hard corner at every transition —
and a bead of plastic has no hard corners.
"""

UNREACHED = 1.4
"""How much wider a corner is drawn once the seam beside it is staggered.

On a flat plane the nozzle's underside passes over the corner and presses it
shut. Raise the loop beside it by half a layer and half of that corner sits
below the nozzle and out of its reach, so it stays open — which is what the
flow is there to pay for, and why the middle panel is the worst of the three
on its own.
"""

SHUT = 0.5
"""Least of the opened corner a fed one is drawn at.

Not zero: the flow narrows the corner, it does not abolish it, and a panel with
no gap at all would claim something the tool does not do. It has to stay small
against the crest of the boundary wave, though: the arcs at a junction sit on
that crest, and a corner deeper than the crest's flat run meets it at an angle
and kinks the outline where it should be smooth.
"""

AIR = 0.16
"""Clear space left either side of a wall, as a share of a bead's width.

Enough that a brick does not sit against the edge of its panel, and no more:
every millimetre of it is repeated three times across the picture, so padding
here is what makes the figure wide rather than tall.
"""

PANEL = 4.9
"""Height of one panel's drawing area, in inches.

The only size given to the figure. Its width follows from the wall's own
aspect and the rest of the layout is measured off it, so tightening `AIR` or
adding a layer changes the picture's proportions instead of leaving air inside
a box of fixed size.
"""

GUTTER = 0.10
"""Inches between two panels — a seam, not a margin."""

SIDE = 0.10
"""Inches outside the two end panels."""

CROWN = 0.64
"""Inches above the panels, for a panel title and the line under it."""

FOOT = 0.08
"""Inches below the panels. The captions are drawn inside the panels."""


def gap_at(cfg: Wall, flow: float, staggered: bool) -> float:
    """How open a bead's corners are drawn, in mm.

    Wider where the seam beside it is staggered and the nozzle can no longer
    reach it, then narrowed by the flow — fully open at the flow the slicer
    meant, down to `SHUT` at the most the dial can ask for on this geometry.
    """
    top = beads.automatic_flow(cfg.height, cfg.width, beads.MAX_EXTRA_FLOW)
    share = 1.0 if top <= 1.0 else 1.0 - (flow - 1.0) / (top - 1.0)
    opened = UNREACHED if staggered else 1.0
    return cfg.height * GAP * opened * max(share, SHUT)


def key_at(cfg: Wall, radius: float, staggered: bool) -> float:
    """How far the fed material bends the boundary between two loops, in mm.

    It is the corner the flow closed — what the opened corner was, less what is
    left of it — because that material did not vanish: it went into the joint.
    So the wave's crest is exactly as deep as the corner it filled, and the two
    bricks seat against each other rather than meeting at a point.
    """
    if not staggered:
        return 0.0
    return max(gap_at(cfg, 1.0, True) - radius, 0.0)


def panel(axes, cfg: Wall, bricked: bool, theme: Theme) -> None:
    raised = beads.raised_loops(cfg.loops) if bricked else [False] * cfg.loops
    # Every raised column climbs alike, so one curve serves every boundary.
    curve = key_curve(
        beads.spans(beads.offsets(cfg.layers, True, cfg.height, cfg.capped), cfg.height),
        cfg.height,
    )

    for bead in wall(cfg, raised):
        if bead["loop"] == 0:
            fill, edge = theme.skin, theme.skin_edge
        elif bead["raised"]:
            fill, edge = theme.lifted, theme.lifted_edge
        else:
            fill, edge = theme.inner, theme.inner_edge
        # The plate layer is never raised, so nothing beside it is out of reach.
        staggered = bricked and bead["layer"] > 0
        radius = gap_at(cfg, bead["flow"], staggered)
        # One boundary, one curve. The column above the line pushes right at
        # its own mid height; its neighbour, half a layer down, pushes left
        # there — so the sign alternates with the parity and the two agree.
        sign = 1.0 if bead["raised"] else -1.0
        # A wave of one layer's period is only in phase where the raised column
        # stands a clean half layer up. While it is climbing its beads are a
        # quarter layer taller and a capped one is half a layer shorter, so the
        # key is taken shallow there rather than dropped: a plain rectangle
        # would read as a wall that is not a wall.
        key = key_at(cfg, radius, staggered) * sign
        axes.add_patch(
            Polygon(
                brick(
                    bead["left"],
                    bead["right"],
                    bead["bottom"],
                    bead["top"],
                    radius,
                    curve,
                    outer=cfg.height * GAP if bead["loop"] == 0 else None,
                    # Only the face of the part is free air. The innermost one
                    # has infill laid against it, so it keys like any other.
                    amp_left=0.0 if bead["loop"] == 0 else -key,
                    amp_right=key,
                ),
                closed=True,
                facecolor=fill,
                edgecolor=edge,
                linewidth=1.0,
                joinstyle="round",
                zorder=3 if bead["raised"] else (2 if bead["loop"] == 0 else 1),
            )
        )

    trace_layer_boundary(axes, cfg, raised, theme)


def trace_layer_boundary(axes, cfg: Wall, raised: list[bool], theme: Theme) -> None:
    """The join between two layers, drawn across the whole wall.

    This is the plane an FDM part splits along. Without bricking it runs
    straight through; with it, it has to climb over every raised column.
    """
    spacing = beads.bead_spacing(cfg.height, cfg.width)
    at = cfg.layers // 2
    seams = []
    for loop, lifts in enumerate(raised):
        standing = beads.offsets(cfg.layers, lifts, cfg.height, cfg.capped)
        seams.append(at * cfg.height + standing[at - 1])

    margin = spacing * 0.62
    edges = [-margin]
    edges += [(a + b) * spacing / 2.0 for a, b in zip(range(cfg.loops), range(1, cfg.loops))]
    edges += [(cfg.loops - 1) * spacing + margin]

    xs, ys = [], []
    for loop, seam in enumerate(seams):
        xs += [edges[loop], edges[loop + 1]]
        ys += [seam, seam]

    axes.plot(
        xs,
        ys,
        color=theme.seam,
        linewidth=2.6,
        linestyle=(0, (5, 3)),
        zorder=4,
        solid_capstyle="butt",
        path_effects=[effects.withStroke(linewidth=4.6, foreground=theme.paper)],
    )


def draw(cfg: Wall, theme: Theme, out: Path) -> Path:
    spacing = beads.bead_spacing(cfg.height, cfg.width)
    flow = beads.automatic_flow(cfg.height, cfg.width, cfg.extra_flow)
    inward = beads.skin_offset(flow, cfg.skin_width, cfg.height)
    face = -cfg.skin_width / 2.0
    top = cfg.layers * cfg.height
    sliced = Wall(**{**cfg.__dict__, "extra_flow": 0.0})

    steps = [
        (
            sliced,
            False,
            "as sliced",
            "",
            "every gap lines up\na channel straight through the wall",
        ),
        (
            sliced,
            True,
            "bricked",
            "what --bricks --extra-flow 0 does",
            "the gaps stagger — but open up\nless bead touches bead: a weaker joint",
        ),
        (
            cfg,
            True,
            "bricked + extra flow",
            "what --bricks does",
            "filled, and keyed into\nmore contact than as sliced, and no channel",
        ),
    ]

    # One range for all three, taken from the widest: the fed panel's innermost
    # bead reaches furthest in, and a clipped brick would read as a cut wall.
    reach = beads.bead_width(flow, spacing, cfg.height) / 2.0
    left = face - cfg.width * AIR
    right = (cfg.loops - 1) * spacing + reach + cfg.width * AIR
    floor, ceiling = -cfg.height * 3.4, top + cfg.height * 1.0

    # The panel is sized to the wall rather than the wall fitted into a panel,
    # so an equal aspect leaves nothing over to pad the picture out with.
    pane_width = PANEL * (right - left) / (ceiling - floor)
    size = (2.0 * SIDE + 3.0 * pane_width + 2.0 * GUTTER, CROWN + PANEL + FOOT)
    figure, panes = plt.subplots(1, 3, figsize=size, facecolor=theme.paper)

    for axes, (shown, bricked, title, switch, caption) in zip(panes, steps):
        axes.set_facecolor(theme.paper)
        panel(axes, shown, bricked, theme)
        axes.set_title(title, color=theme.ink, fontsize=15, pad=18, fontweight="bold")
        axes.set_xlim(left, right)
        axes.set_ylim(floor, ceiling)
        axes.set_aspect("equal")
        axes.axis("off")

        if switch:
            axes.text(
                0.5,
                1.012,
                switch,
                transform=axes.transAxes,
                ha="center",
                va="bottom",
                color=theme.faint,
                fontsize=SWITCH_SIZE,
            )

        # The face the eye lands on. It is on the same line in all three, which
        # is the point: the visible wall's gain goes inward, never outward.
        axes.plot(
            [face, face],
            [-cfg.height * 0.75, top + cfg.height * 0.4],
            color=theme.faint,
            linewidth=1.1,
            linestyle=(0, (2, 3)),
            zorder=0,
        )
        # Anchored on its left, not centred: the face is the panel's left edge,
        # so a centred label hangs half of itself outside the figure.
        axes.text(
            face + cfg.width * AIR / 3.0,
            -cfg.height * 1.15,
            "outer face",
            color=theme.faint,
            fontsize=9.5,
            ha="left",
            va="center",
        )
        axes.text(
            (left + right) / 2.0,
            -cfg.height * 2.35,
            caption,
            ha="center",
            va="center",
            color=theme.ink,
            fontsize=11.5,
            linespacing=1.5,
        )

    # Nothing is written under the panels: README.md carries the explanation,
    # where it can be read at any width and searched.
    edge, foot = SIDE / size[0], FOOT / size[1]
    figure.subplots_adjust(
        left=edge,
        right=1.0 - edge,
        top=1.0 - CROWN / size[1],
        bottom=foot,
        wspace=GUTTER / pane_width,
    )

    # The third panel is the tool's actual output; the first two are how it got
    # there.
    box = panes[2].get_position()
    figure.add_artist(
        FancyBboxPatch(
            (box.x0 - edge / 3.0, foot / 2.0),
            box.width + 2.0 * edge / 3.0,
            1.0 - foot,
            boxstyle="round,pad=0,rounding_size=0.012",
            transform=figure.transFigure,
            facecolor=theme.highlight,
            edgecolor=theme.highlight_edge,
            linewidth=1.2,
            zorder=-1,
        )
    )

    # One line for all three, so the eye can see that the course sitting on the
    # bed is the same course in each. Above the panel tint, under the beads.
    # The draw is what applies the equal aspect: without it the axes box is
    # still the pre-aspect one and the line lands a fraction of a layer low.
    figure.canvas.draw()
    plate = figure.transFigure.inverted().transform(panes[0].transData.transform((0.0, 0.0)))[1]
    figure.add_artist(
        Line2D(
            [edge / 2.0, 1.0 - edge / 2.0],
            [plate, plate],
            transform=figure.transFigure,
            color=theme.faint,
            linewidth=1.2,
            linestyle=(0, (4, 3)),
            zorder=-0.5,
        )
    )
    figure.text(
        1.0 - 1.5 * edge,
        plate - 0.008,
        "heated bed",
        ha="right",
        va="top",
        color=theme.faint,
        fontsize=9.5,
    )

    out.parent.mkdir(parents=True, exist_ok=True)
    figure.savefig(out, dpi=200, facecolor=theme.paper)
    plt.close(figure)
    return out


def main() -> None:
    here = Path(__file__).resolve().parents[4]
    parse = argparse.ArgumentParser(description=__doc__)
    parse.add_argument("--output-dir", type=Path, default=here / "img")
    parse.add_argument("--stem", default="interlock")
    parse.add_argument("--layers", type=int, default=10)
    parse.add_argument("--loops", type=int, default=5)
    parse.add_argument("--height", type=float, default=beads.REFERENCE_HEIGHT)
    parse.add_argument("--width", type=float, default=beads.REFERENCE_WIDTH)
    parse.add_argument("--skin-width", type=float, default=beads.REFERENCE_NOZZLE)
    parse.add_argument(
        "--extra-flow",
        type=float,
        default=beads.MAX_EXTRA_FLOW,
        help="as a fraction, like Config::extra_flow. The shipped default of "
        "0.05 moves the visible wall by 4 µm, which no picture can show, so "
        "the diagram is drawn at the top of the dial — a setting the binary "
        "accepts, not an invented exaggeration.",
    )
    parse.add_argument("--capped", action="store_true", default=False)
    parse.add_argument("--no-capped", dest="capped", action="store_false")
    args = parse.parse_args()

    cfg = Wall(
        height=args.height,
        width=args.width,
        skin_width=args.skin_width,
        loops=args.loops,
        layers=args.layers,
        extra_flow=args.extra_flow,
        capped=args.capped,
    )
    for theme in (LIGHT, DARK):
        written = draw(cfg, theme, args.output_dir / f"{args.stem}-{theme.name}.png")
        print(written)


if __name__ == "__main__":
    main()
