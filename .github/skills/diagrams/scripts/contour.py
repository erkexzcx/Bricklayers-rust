#!/usr/bin/env python3
"""Draw a shallow slope's cross-section as sliced and as followed, one over the
other.

Every coordinate comes out of `surface.py` and `beads.py`, which mirror
`src/zaa/surface.rs`, `src/zaa.rs` and `src/brick.rs`, so the picture is the
binary's own arithmetic rather than an artist's impression of it. Run `pin.py`
first if you have touched either side.

    python3 contour.py --output-dir img

writes `contour-light.png` and `contour-dark.png`, the pair README.md switches
between with `<picture>`.
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
import surface  # noqa: E402
from render import DARK, GAP, LIGHT, SWITCH_SIZE, Theme, brick  # noqa: E402

FLAT = staticmethod(lambda _: 0.0)
"""No wave on any side. The joints here are between layers, not between loops:
nothing keys sideways, so every bead is a plain stadium."""

BODY = 2
"""Courses drawn below the lowest whole tread.

Enough that the bottom of the frame is solid across and the slope reads as
carrying on rather than as a staircase standing on nothing.
"""

AIR = 0.55
"""Clear space above the highest bead, as a share of a layer height."""

FLOOR = 0.12
"""Clear space below the lowest layer, as a share of a layer height."""

PANEL = 1.62
"""Height of one panel's drawing area, in inches.

The only size given to the figure: its width follows from the slope's own
aspect, so a shallower slope makes a wider picture instead of leaving air
inside a box of fixed size.
"""

GUTTER = 0.30
"""Inches between the two panels."""

SIDE = 0.10
"""Inches outside the panels, left and right."""

CROWN = 0.96
"""Inches above each panel, for its title and the line that explains it."""

FOOT = 0.08
"""Inches below the lower panel."""


@dataclass(frozen=True)
class Slope:
    """The geometry one panel is drawn from."""

    height: float
    width: float
    skin_width: float
    strip: float
    treads: int
    reach: float

    @property
    def layers(self) -> int:
        return self.treads + BODY

    @property
    def spacing(self) -> float:
        return beads.bead_spacing(self.height, self.width)

    @property
    def skin_spacing(self) -> float:
        return beads.bead_spacing(self.height, self.skin_width)

    @property
    def first(self) -> float:
        """Where the lowest drawn layer's outline runs.

        Zero, so every other length in the figure is measured from it.
        """
        return 0.0

    @property
    def left(self) -> float:
        """The frame's left edge, half a tread below the lowest whole one.

        On a step rather than mid-tread it would cut a course where the lattice
        of beads and the edge of a strip disagree, leaving one bead of the layer
        below half exposed at the very edge. Mid-tread there is nothing to cut:
        the strip below it ends a whole half tread further left.
        """
        return surface.outline(BODY, self.first, self.strip) - self.strip / 2.0

    @property
    def right(self) -> float:
        """The frame's right edge, half a tread past the highest whole one.

        Cut on a step instead and the top course's own last bead — the one the
        course above covers — would sit there with nothing drawn over it,
        reading as a notch the transform had put in the surface.
        """
        return surface.outline(self.layers, self.first, self.strip) + self.strip / 2.0

    @property
    def degrees(self) -> float:
        return math.degrees(math.atan(self.height / self.strip))


def course(cfg: Slope, layer: int, followed: bool) -> list[dict]:
    """Every bead of one layer, as the ground it owns rather than as its width.

    A bead is laid closer to its neighbour than it is wide, so what each one
    owns is the ground between the midpoints to either side. Only the visible
    wall's outer face reaches its own half width, because that face is free air
    and nothing tiles against it.

    The bottom of every bead is its layer's plane less one layer: what is under
    a strip is covered by this layer, so it was laid flat. That is the whole
    reason a stretch can be metered for `height + rise` and fill exactly.
    """
    edge = surface.outline(layer, cfg.first, cfg.strip)
    face = surface.face_of(edge, cfg.skin_width)
    plane = surface.plane_of(layer, cfg.height)
    reach = int((cfg.right - edge) / cfg.spacing) + 2

    drawn = []
    for at, centre in enumerate(surface.centres(edge, cfg.spacing, reach)):
        # Distance from where this layer stops being inside itself, which is
        # what `surface.rs` measures on the footprint grid.
        out = centre - face
        open_ = out < cfg.strip
        # The visible wall may be lowered onto the surface and never lifted.
        ceiling = surface.WALL_CEILING if at == 0 else math.inf
        stands = (
            surface.rise(out, cfg.strip, out + cfg.strip, cfg.height, cfg.reach, ceiling)
            if followed and open_
            else 0.0
        )
        top = plane + stands
        bottom = plane - cfg.height
        drawn.append(
            {
                "left": (
                    centre - beads.bead_width(1.0, cfg.skin_spacing, top - bottom) / 2.0
                    if at == 0
                    else centre - cfg.spacing / 2.0
                ),
                "right": centre + cfg.spacing / 2.0,
                "bottom": bottom,
                "top": top,
                "centre": centre,
                "wall": at == 0,
                "open": open_,
                "moved": stands != 0.0,
            }
        )
    return drawn


def panel(axes, cfg: Slope, followed: bool, theme: Theme) -> None:
    radius = cfg.height * GAP
    # One course past the frame, so the top one has something covering the bead
    # its own strip does not reach.
    for layer in range(cfg.layers + 1):
        for bead in course(cfg, layer, followed):
            if bead["wall"]:
                fill, edge = theme.skin, theme.skin_edge
            elif bead["moved"]:
                fill, edge = theme.lifted, theme.lifted_edge
            else:
                fill, edge = theme.inner, theme.inner_edge
            axes.add_patch(
                Polygon(
                    brick(
                        bead["left"],
                        bead["right"],
                        bead["bottom"],
                        bead["top"],
                        radius,
                        FLAT,
                        # The face of the part is free air: no neighbour tiles
                        # against it, so it keeps the profile the slicer gave
                        # it whatever the bead behind it is doing.
                        outer=radius if bead["wall"] else None,
                    ),
                    closed=True,
                    facecolor=fill,
                    edgecolor=edge,
                    linewidth=0.9,
                    joinstyle="round",
                    zorder=3 if bead["wall"] else (2 if bead["moved"] else 1),
                )
            )

    trace_surface(axes, cfg, theme)


def trace_surface(axes, cfg: Slope, theme: Theme) -> None:
    """Where the model's surface really is, drawn across the whole slope.

    One straight line, because every strip is measured against the same one.
    As sliced it cuts through each course; followed, every bead's top lands on
    it — which is the argument the figure exists to make.
    """
    face = surface.face_of(surface.outline(0, cfg.first, cfg.strip), cfg.skin_width)
    ends = [cfg.left, cfg.right]
    axes.plot(
        ends,
        [surface.surface(x, face, cfg.strip, cfg.height) for x in ends],
        color=theme.seam,
        linewidth=2.2,
        linestyle=(0, (5, 3)),
        zorder=4,
        solid_capstyle="butt",
        path_effects=[effects.withStroke(linewidth=4.2, foreground=theme.paper)],
    )


def draw(cfg: Slope, theme: Theme, out: Path) -> Path:
    steps = [
        (
            False,
            "as sliced",
            "a course is laid at one height, so the surface is missed by half a layer",
        ),
        (
            True,
            "followed",
            "each bead sits where the surface is, metered for its own gap",
        ),
    ]

    left, right = cfg.left, cfg.right
    floor = -cfg.height * FLOOR
    # The highest bead of the course drawn past the frame stands half a layer
    # over its own plane.
    ceiling = surface.plane_of(cfg.layers, cfg.height) + cfg.height * (0.5 + AIR)

    pane = PANEL * (right - left) / (ceiling - floor)
    size = (2.0 * SIDE + pane, FOOT + 2.0 * (PANEL + CROWN) + GUTTER)
    figure = plt.figure(figsize=size, facecolor=theme.paper)

    # Placed by hand rather than by a grid: both panels are the same size and
    # every gap between them is stated in inches, so nothing is left to a
    # layout engine to crop.
    edge, foot = SIDE / size[0], FOOT / size[1]
    across, tall = pane / size[0], PANEL / size[1]
    bottoms = [(FOOT + PANEL + CROWN + GUTTER) / size[1], foot]
    panes = [figure.add_axes((edge, bottom, across, tall)) for bottom in bottoms]

    for axes, (followed, title, caption) in zip(panes, steps):
        axes.set_facecolor(theme.paper)
        panel(axes, cfg, followed, theme)
        axes.set_title(title, color=theme.ink, fontsize=15, pad=50, fontweight="bold")
        axes.set_xlim(left, right)
        axes.set_ylim(floor, ceiling)
        axes.set_aspect("equal")
        axes.axis("off")

        # Nothing is written inside the panel: the slope fills it corner to
        # corner, and a caption over the drawing would cross the very line the
        # figure is about.
        axes.text(
            0.5,
            1.02,
            caption,
            transform=axes.transAxes,
            ha="center",
            va="bottom",
            color=theme.ink,
            fontsize=11,
        )
        if followed:
            axes.text(
                0.5,
                1.20,
                "what --zaa does",
                transform=axes.transAxes,
                ha="center",
                va="bottom",
                color=theme.faint,
                fontsize=SWITCH_SIZE,
            )
        else:
            axes.legend(
                handles=[
                    Line2D(
                        [],
                        [],
                        color=theme.seam,
                        linewidth=2.2,
                        linestyle=(0, (5, 3)),
                        label="the model's surface",
                    )
                ],
                loc="upper left",
                frameon=False,
                handlelength=2.4,
                labelcolor=theme.ink,
                fontsize=10.5,
            )

    # The lower panel is the transform's actual output; the upper one is what
    # it was handed.
    figure.add_artist(
        FancyBboxPatch(
            (edge / 3.0, foot / 2.0),
            across + 4.0 * edge / 3.0,
            (FOOT / 2.0 + PANEL + CROWN) / size[1],
            boxstyle="round,pad=0,rounding_size=0.006",
            transform=figure.transFigure,
            facecolor=theme.highlight,
            edgecolor=theme.highlight_edge,
            linewidth=1.2,
            zorder=-1,
        )
    )

    out.parent.mkdir(parents=True, exist_ok=True)
    figure.savefig(out, dpi=200, facecolor=theme.paper)
    plt.close(figure)
    return out


def main() -> None:
    here = Path(__file__).resolve().parents[4]
    parse = argparse.ArgumentParser(description=__doc__)
    parse.add_argument("--output-dir", type=Path, default=here / "img")
    parse.add_argument("--stem", default="contour")
    parse.add_argument("--treads", type=int, default=3)
    parse.add_argument("--height", type=float, default=beads.REFERENCE_HEIGHT)
    parse.add_argument("--width", type=float, default=beads.REFERENCE_WIDTH)
    parse.add_argument("--skin-width", type=float, default=beads.REFERENCE_NOZZLE)
    parse.add_argument(
        "--strip",
        type=float,
        default=1.6,
        help="width of one tread in mm, which is the layer height over the "
        "tangent of the slope. The default is a 7.1 degree slope at 0.2 mm "
        "layers — shallow enough to stair-step badly, steep enough that the "
        "walls cover the tread, and well inside the reach those layers derive.",
    )
    parse.add_argument(
        "--reach",
        type=float,
        default=surface.reach_for(0.2),
        help="widest tread followed, in mm. The binary has no dial for this: "
        "it derives it per layer as the tread a SHALLOWEST_SLOPE surface "
        "leaves. Here so a figure can show the fade.",
    )
    args = parse.parse_args()

    cfg = Slope(
        height=args.height,
        width=args.width,
        skin_width=args.skin_width,
        strip=args.strip,
        treads=args.treads,
        reach=args.reach,
    )
    print(f"{cfg.degrees:.1f} degrees, {cfg.strip / cfg.spacing:.1f} beads per tread")
    for theme in (LIGHT, DARK):
        written = draw(cfg, theme, args.output_dir / f"{args.stem}-{theme.name}.png")
        print(written)


if __name__ == "__main__":
    main()
