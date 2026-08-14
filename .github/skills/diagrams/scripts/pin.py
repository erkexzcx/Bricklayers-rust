#!/usr/bin/env python3
"""Check the models the diagrams are drawn from against the Rust they mirror.

`beads.py` mirrors `src/brick.rs`; `surface.py` mirrors `src/zaa/surface.rs` and
`src/zaa.rs`. Two kinds of check for each, because either alone lets a change
through:

1. **Constants.** Every number the Python holds is read back out of the Rust
   and compared. A constant renamed or retuned on one side fails here.
2. **Output.** A synthetic slice is put through the real compiled binary and
   what it emits is compared with what the Python predicts. This is what
   catches a formula that is right in isolation and wrong in place.

    python3 pin.py            # builds the binary if needed
    python3 pin.py --binary target/release/corbel

Exit status is 0 when the models and the binary agree.
"""

from __future__ import annotations

import argparse
import re
import statistics
import subprocess
import sys
import tempfile
from pathlib import Path

import beads
import surface

ROOT = Path(__file__).resolve().parents[4]
BRICK = ROOT / "src" / "brick.rs"
FIELD = ROOT / "src" / "zaa" / "surface.rs"
CONTOUR = ROOT / "src" / "zaa.rs"

TOLERANCE = 5e-4
"""Half a micron either way: a coordinate is written to three decimals."""


def rust_constant(source: str, name: str, where: Path = BRICK) -> float:
    found = re.search(rf"(?:pub )?const {name}: f64 = ([-0-9._eE]+);", source)
    if not found:
        raise SystemExit(f"{name} is gone from {where.name}")
    return float(found.group(1).replace("_", ""))


def rust_usize(source: str, name: str) -> int:
    found = re.search(rf"const {name}: usize = ([0-9]+);", source)
    if not found:
        raise SystemExit(f"{name} is gone from src/brick.rs")
    return int(found.group(1))


def check_constants(source: str) -> list[str]:
    wrong = []
    pairs = [
        ("DEFAULT_EXTRA_FLOW", beads.DEFAULT_EXTRA_FLOW),
        ("MIN_EXTRA_FLOW", beads.MIN_EXTRA_FLOW),
        ("MAX_EXTRA_FLOW", beads.MAX_EXTRA_FLOW),
        ("REFERENCE_NOZZLE", beads.REFERENCE_NOZZLE),
        ("REFERENCE_HEIGHT", beads.REFERENCE_HEIGHT),
        ("REFERENCE_WIDTH", beads.REFERENCE_WIDTH),
    ]
    for name, mine in pairs:
        theirs = rust_constant(source, name)
        if theirs != mine:
            wrong.append(f"{name}: brick.rs says {theirs}, beads.py says {mine}")
    ramp = rust_usize(source, "RAMP")
    if ramp != beads.RAMP:
        wrong.append(f"RAMP: brick.rs says {ramp}, beads.py says {beads.RAMP}")
    if "MAX_WALL_FLOW" in source:
        wrong.append("MAX_WALL_FLOW is back in brick.rs; the ceiling is derived")
    return wrong


def check_surface_constants(field: str, contour: str) -> list[str]:
    wrong = []
    for name, mine, source, where in [
        ("FADE", surface.FADE, field, FIELD),
        ("STEPS", surface.STEPS, field, FIELD),
        ("SLOPE_MARGIN", surface.SLOPE_MARGIN, field, FIELD),
        ("SHALLOWEST_SLOPE", surface.SHALLOWEST_SLOPE, contour, CONTOUR),
    ]:
        theirs = rust_constant(source, name, where)
        if theirs != mine:
            wrong.append(f"{name}: {where.name} says {theirs}, surface.py says {mine}")
    # The sampling step is a share of whatever grid the file was given rather
    # than a distance, so it is pinned by the line that spends it as well as by
    # its own value.
    if "let along_grid = self.grid.cell() * STEP;" not in contour:
        wrong.append("zaa.rs no longer samples in units of its own grid cell")
    if rust_constant(contour, "STEP", CONTOUR) != surface.STEP_OF_A_CELL:
        wrong.append(f"surface.py samples {surface.STEP_OF_A_CELL} of a cell")
    for gone in ("DEFAULT_REACH", "DEFAULT_RESOLUTION"):
        if gone in contour:
            wrong.append(f"{gone} is back in zaa.rs; both are derived")
    # Where the fade runs is not a constant at all. `FADE` says only how wide
    # the taper is; whether it runs inward from the reach or outward past it is
    # in the expression, and moving it changed no number on either side. That
    # is exactly how the Python was left drawing the old inward taper against a
    # binary that had stopped using it, so the expression is pinned here and
    # the output check below puts a tread through the fade itself.
    if "let carried = reach * (1.0 + FADE);" not in field:
        wrong.append(f"{FIELD.name} no longer carries the fade past the reach")
    if "((carried - strip) / (reach * FADE))" not in field:
        wrong.append(f"{FIELD.name} fades on something other than the strip's width")
    # The visible wall's ceiling is not a named constant, so it is pinned by the
    # line that applies it and by the test that holds it.
    if "Feature::ExternalPerimeter => 0.0" not in contour:
        wrong.append("zaa.rs no longer caps the visible wall at its own plane")
    if surface.WALL_CEILING != 0.0:
        wrong.append(f"surface.py caps the visible wall at {surface.WALL_CEILING}")
    return wrong


HEIGHT = 0.2
WIDTH = 0.45
SKIN = 0.4
SIDE = 20.0
PLANES = [0.2, 0.4, 0.6, 0.8, 1.0]


def synthetic() -> str:
    """A PrusaSlicer-shaped slice, the same shape `tests/end_to_end.rs` uses.

    Walls alone are not enough: with nothing printed over them the survey finds
    no cell covering a loop and caps every column, which measures the wrong
    thing. The widths are stated so the flow is derived rather than guessed.
    """
    text = [
        "; generated by PrusaSlicer",
        "M83 ; extruder relative mode",
        f"; layer_height = {HEIGHT}",
        f"; perimeter_extrusion_width = {WIDTH}",
        f"; external_perimeter_extrusion_width = {SKIN}",
    ]
    for at, z in enumerate(PLANES):
        text.append(";LAYER_CHANGE")
        text.append(f"G1 Z{z:.3f} F9000")
        text.append(";TYPE:External perimeter")
        text.append("G1 X0 Y0 F9000")
        for x, y in [(SIDE, 0.0), (SIDE, SIDE), (0.0, SIDE), (0.0, 0.0)]:
            text.append(f"G1 X{x:.3f} Y{y:.3f} E0.66000 ; skin")
        text.append(";TYPE:Perimeter")
        for inset in (0.45, 0.90):
            text.append(f"G1 X{inset:.3f} Y{inset:.3f} F9000")
            far = SIDE - inset
            for x, y in [(far, inset), (far, far), (inset, far), (inset, inset)]:
                text.append(f"G1 X{x:.3f} Y{y:.3f} E0.64000")
        solid = at == 0 or at + 1 == len(PLANES)
        text.append(";TYPE:Solid infill" if solid else ";TYPE:Internal infill")
        text.append("G1 X2 Y2 F9000")
        text.append("G1 X18 Y18 E1.20000")
    text.append("M104 S0")
    return "\n".join(text) + "\n"


def build(binary: Path | None) -> Path:
    if binary:
        return binary
    # Always, not only when the file is missing: a stale release build is
    # exactly the drift this script exists to catch, and it would report the
    # Rust as wrong.
    subprocess.run(
        ["cargo", "build", "--release"], cwd=ROOT, check=True, capture_output=True
    )
    return ROOT / "target" / "release" / "corbel"


def check_output(binary: Path, extra: float) -> list[str]:
    """Run the binary over the synthetic slice and compare what it emits."""
    wrong = []
    with tempfile.TemporaryDirectory() as room:
        source = Path(room) / "pin.gcode"
        source.write_text(synthetic(), encoding="utf-8")
        result = subprocess.run(
            [
                str(binary),
                "--bricks",
                "--extra-flow",
                f"{extra * 100:g}",
                str(source),
            ],
            capture_output=True,
            text=True,
        )
        if result.returncode != 0:
            return [f"the binary refused the fixture: {result.stderr.strip()}"]
        produced = source.read_text(encoding="utf-8")

    flow = beads.automatic_flow(HEIGHT, WIDTH, extra)
    inward = beads.skin_offset(flow, SKIN, HEIGHT)
    predicted = f"G1 X{SIDE - inward:.3f} Y{inward:.3f} "
    if predicted not in produced:
        found = [line for line in produced.splitlines() if line.endswith("; skin")]
        wrong.append(
            f"the visible wall should be drawn in to {predicted.strip()!r}, got "
            f"{found[len(found) // 2] if found else 'no visible wall at all'!r}"
        )

    # The layer on the plate is never raised and the top one is capped, so the
    # columns that stand are the ones between them.
    raises = [
        float(found)
        for found in re.findall(r"Z([0-9.]+) ; corbel brick raised", produced)
    ]
    expected = [PLANES[at] + beads.rise(at, HEIGHT) for at in range(1, len(PLANES) - 1)]
    if len(raises) != len(expected):
        wrong.append(f"expected {len(expected)} raised columns, got {len(raises)}")
    for at, (got, want) in enumerate(zip(raises, expected), start=1):
        if abs(got - want) > TOLERANCE:
            wrong.append(f"raise {at}: binary put it at {got}, the model says {want}")
    return wrong


CELL = surface.CELL
"""The footprint grid, in mm, from `src/geometry/footprint.rs`."""

STRIP = 8.0
"""Tread of the synthetic slope, in mm.

Far wider than a real one on purpose. The binary measures its distances on the
footprint grid and the model measures them exactly, so the disagreement between
them is a fixed fraction of a cell — and a wide tread makes that a small share
of the half layer the rise spans, which is what leaves the check some teeth.
"""

REACH = surface.reach_for(HEIGHT)
"""What the binary derives for this fixture's layers, in mm.

Nothing passes it on the command line — there is nothing to pass. `STRIP` is
under three quarters of it, so that wedge is followed at full amplitude; the
one below is drawn wider on purpose so the fade is measured too.
"""

FADED = REACH * (1.0 + surface.FADE / 2.0)
"""A tread half way through the fade, in mm.

Wider than the reach, so it is followed at half amplitude rather than at full
or not at all — which is the one part of the surface model no constant pins and
no other fixture reaches. A model that fades inward from the reach calls this
tread flat and predicts the plane; a model that fades outward past it predicts
half the swing. The two therefore disagree by a quarter of a layer at the edges
of a tread and by an eighth of one averaged across it, against a slack of two
hundredths of a layer — so the median alone fails, and the worst of them fails
again.
"""

SLOPE_LAYERS = 6
SLOPE_DEPTH = 60.0
SLOPE_FIRST = 70.0


def first_side(strip: float) -> float:
    """The lowest layer's own outline, in mm along the slope.

    Enough that the topmost layer still has a tread of its own to fill: its
    face stands `(SLOPE_LAYERS - 1)` treads inside this, and the fill starts
    one tread inside that again.
    """
    return max(SLOPE_FIRST, SLOPE_LAYERS * strip + WIDTH)


def middle_of(strip: float) -> float:
    """How far from an end of the wedge the comparison starts, in mm.

    Within one tread of an end, that end is the nearest boundary rather than the
    sloping face; within two, it is the nearest boundary of the layer *below*, so
    `sloped` falls away and the surface is correctly left flatter than a one-
    distance model predicts. Measured on the wedge: comparing at 8.8 mm from the
    end has the binary 0.079 mm below the model and the binary is the one that is
    right. Past two treads neither can bind and the model describes it exactly.
    """
    return 2.0 * strip


def depth_of(strip: float) -> float:
    """How far the wedge runs along its own ridge, in mm.

    Two treads at each end are given up to the ends themselves, so a depth in
    treads rather than in millimetres is what keeps a band left in the middle
    to compare: the same three and a half treads of it whatever the tread is.
    Without this a wider tread eats its own comparison window and the check
    passes by having nothing left to look at.
    """
    return max(SLOPE_DEPTH, 7.5 * strip)


def sloped(strip: float) -> str:
    """A wedge: every layer one tread shorter in +x than the one below it.

    Only the +x face slopes. The other three are vertical, so the layer above
    covers all but a band one tread wide along that face, and the rise inside
    it follows one distance rather than a corner's two.
    """
    first, depth = first_side(strip), depth_of(strip)
    text = [
        "; generated by PrusaSlicer",
        "M83 ; extruder relative mode",
        f"; layer_height = {HEIGHT}",
        f"; perimeter_extrusion_width = {WIDTH}",
        f"; external_perimeter_extrusion_width = {SKIN}",
    ]
    for at in range(SLOPE_LAYERS):
        far = first - at * strip
        text.append(";LAYER_CHANGE")
        text.append(f"G1 Z{(at + 1) * HEIGHT:.3f} F9000")
        text.append(";TYPE:External perimeter")
        text.append("G1 X0.000 Y0.000 F9000")
        for x, y in [(far, 0.0), (far, depth), (0.0, depth), (0.0, 0.0)]:
            text.append(f"G1 X{x:.3f} Y{y:.3f} E1.00000")
        # Filled along y, so one move samples one distance from the sloping
        # face all the way down it and comes out at a single height.
        text.append(";TYPE:Top solid infill")
        step = far - strip + WIDTH
        while step < far - WIDTH:
            text.append(f"G1 X{step:.3f} Y2.000 F9000")
            text.append(f"G1 X{step:.3f} Y{depth - 2.0:.3f} E1.00000")
            step += WIDTH
    text.append("M104 S0")
    return "\n".join(text) + "\n"


def followed(produced: str) -> list[tuple[int, str, float, float, float]]:
    """Every height the surface transform wrote, with its layer and region.

    A move that is followed becomes several, and only the first of them carries
    the stamp — the rest are recognised by naming a Z at all, which nothing
    else inside a region does. Reading the stamp alone samples whichever piece
    the splitter happened to cut first, and where that lands is an artefact of
    the grid rather than of the surface.

    Each piece is reported at its **middle**. A piece is written at one height
    over its whole length, so that is the point the height belongs to; reading
    it at the far end puts a long piece's answer wherever it stopped.
    """
    found = []
    layer, region, x, y, z = -1, "", 0.0, 0.0, 0.0
    for line in produced.splitlines():
        if line.startswith(";LAYER_CHANGE"):
            layer += 1
        elif line.startswith(";TYPE:"):
            region = line[len(";TYPE:") :]
        elif line.startswith("G1 "):
            across = re.search(r" X(-?[0-9.]+)", line)
            along = re.search(r" Y(-?[0-9.]+)", line)
            up = re.search(r" Z(-?[0-9.]+)", line)
            lays = re.search(r" E(-?[0-9.]+)", line)
            was = (x, y)
            x = float(across.group(1)) if across else x
            y = float(along.group(1)) if along else y
            z = float(up.group(1)) if up else z
            if up and lays:
                found.append((layer, region, (was[0] + x) / 2, (was[1] + y) / 2, z))
    return found


def check_surface_output(binary: Path, strip: float) -> list[str]:
    """Put a wedge of this tread through the binary and compare heights."""
    wrong = []
    tread = f"a {strip:.2f} mm tread"
    with tempfile.TemporaryDirectory() as room:
        source = Path(room) / "slope.gcode"
        source.write_text(sloped(strip), encoding="utf-8")
        result = subprocess.run(
            [str(binary), "--zaa", str(source)],
            capture_output=True,
            text=True,
        )
        if result.returncode != 0:
            return [f"the binary refused {tread}: {result.stderr.strip()}"]
        produced = source.read_text(encoding="utf-8")

    moves = followed(produced)
    if not moves:
        return [f"the binary followed nothing on {tread}, which it should have"]

    # One cell of grid either way: the binary reads its distances off the
    # footprint and the model reads them exactly.
    slack = HEIGHT * CELL / strip
    first, middle = first_side(strip), middle_of(strip)
    depth = depth_of(strip)
    off, capped = [], 0
    for layer, region, x, y, z in moves:
        plane = surface.plane_of(layer, HEIGHT)
        if layer + 1 >= SLOPE_LAYERS:
            wrong.append(f"layer {layer} has a flat top and was followed anyway")
            continue
        if region == "External perimeter" and z > plane + TOLERANCE:
            capped += 1
        # Within two treads of an end, an end is nearer than the sloping face
        # and the rise there follows a second distance. The model here follows
        # one, so the ends are left to the audit script.
        if not middle < y < depth - middle:
            continue
        face = surface.outline(layer, first, -strip)
        want = plane + surface.rise(face - x, strip, face - x + strip, HEIGHT, REACH)
        off.append(abs(z - want))
    if capped:
        wrong.append(
            f"{capped} beads of the visible wall were written above their plane "
            f"on {tread}"
        )
    if not off:
        wrong.append(
            f"nothing was followed on {tread} where the sloping face is the "
            "nearest edge"
        )
    if off and statistics.median(off) > slack:
        wrong.append(
            f"on {tread} the median height is {statistics.median(off):.4f} mm off "
            f"the model, past the {slack:.4f} mm the grid can account for"
        )
    if off and max(off) > 3.0 * slack:
        wrong.append(f"on {tread} the worst height is {max(off):.4f} mm off the model")
    return wrong


def main() -> int:
    parse = argparse.ArgumentParser(description=__doc__)
    parse.add_argument("--binary", type=Path, default=None)
    parse.add_argument("--extra-flow", type=float, default=beads.MAX_EXTRA_FLOW)
    args = parse.parse_args()

    source = BRICK.read_text(encoding="utf-8")
    binary = build(args.binary)
    wrong = check_constants(source)
    wrong += check_output(binary, args.extra_flow)
    wrong += check_surface_constants(
        FIELD.read_text(encoding="utf-8"), CONTOUR.read_text(encoding="utf-8")
    )
    # Twice: one tread well inside the reach, where the fade cannot show, and
    # one half way through it, where a fade running the wrong way is the whole
    # of the answer rather than a correction to it.
    wrong += check_surface_output(binary, STRIP)
    wrong += check_surface_output(binary, FADED)

    for line in wrong:
        print(f"drift: {line}", file=sys.stderr)
    if wrong:
        return 1
    print("beads.py and surface.py agree with the Rust and with the binary")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
