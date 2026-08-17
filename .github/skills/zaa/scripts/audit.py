#!/usr/bin/env python3
"""Check what Z anti-aliasing did to a file, against the file it came from.

    audit.py surface  IN.gcode OUT.gcode   what moved, and by how much
    audit.py invariant IN.gcode OUT.gcode  nothing is commanded where it cannot go
    audit.py flow      IN.gcode OUT.gcode  every stretch is metered for its own gap
    audit.py cover     IN.gcode            how much of each layer is exposed

Exits non-zero when a check fails, so it can gate a change.

The plane of a layer is taken from the INPUT, as the lowest height that layer
commands there. Reading it from the output is exactly what this transform makes
meaningless: the output no longer sits on one height, and a run that took every
surface down half a layer would still look flat against its own minimum.
"""

from __future__ import annotations

import math
import re
import sys
from collections import defaultdict
from dataclasses import dataclass, field

LAYER = re.compile(r"^;\s*(LAYER_CHANGE|CHANGE_LAYER|LAYER:\S+)\s*$", re.I)
REGION = re.compile(r"^;\s*(?:TYPE:|FEATURE:)\s*(.+?)\s*$", re.I)
WORD = re.compile(r"\b([XYZEFIJ])(-?\d*\.?\d+)")
MOVE = re.compile(r"^G([0-3])\b")

SURFACE = (
    "top",
    "ironing",
    "skin",
    "outer wall",
    "external perimeter",
    "wall-outer",
    "inner wall",
    "internal perimeter",
    "wall-inner",
    "perimeter",
)
STAMP = "corbel zaa"


@dataclass
class Move:
    """One extruding move, with the axes it left unnamed carried forward."""

    layer: int
    region: str
    x: float
    y: float
    z: float
    from_z: float
    e: float
    length: float
    raw: str
    stamped: bool

    def is_surface(self) -> bool:
        return any(word in self.region for word in SURFACE)

    def middle(self) -> float:
        """The height half way along, which is what the gap under it averages.

        A stretch that climbs crosses a gap that grows evenly with it, so the
        material it needs is the gap under its middle times its length. Read
        at the far end instead, a ramp reads as needing what only its last
        point does — an error that grows with the amplitude and so looks like
        a regression every time the transform gets better at its job.
        """
        return (self.from_z + self.z) / 2.0


@dataclass
class Read:
    moves: list[Move] = field(default_factory=list)
    planes: dict[int, float] = field(default_factory=dict)
    layers: int = 0


def words(line: str) -> dict[str, float]:
    body = line.split(";", 1)[0]
    return {letter: float(value) for letter, value in WORD.findall(body)}


def read(path: str) -> Read:
    out = Read()
    layer, region, relative = -1, "", False
    at = [0.0, 0.0, 0.0]
    position = 0.0
    planes: dict[int, float] = {}

    with open(path, "r", errors="replace") as handle:
        for raw in handle:
            line = raw.rstrip("\n").rstrip("\r")
            stripped = line.strip()
            if stripped.startswith(";"):
                if LAYER.match(stripped):
                    layer += 1
                    region = ""
                    continue
                found = REGION.match(stripped)
                if found:
                    region = found.group(1).lower()
                continue
            if stripped.startswith("M83"):
                relative = True
                continue
            if stripped.startswith("M82"):
                relative = False
                continue
            found = MOVE.match(stripped)
            if not found:
                if stripped.startswith("G92"):
                    e = words(stripped).get("E")
                    if e is not None:
                        position = e
                continue

            given = words(stripped)
            before = tuple(at)
            for index, letter in enumerate("XYZ"):
                if letter in given:
                    at[index] = given[letter]
            if "Z" in given and layer >= 0:
                planes[layer] = min(planes.get(layer, given["Z"]), given["Z"])

            e = given.get("E")
            if e is None:
                continue
            delta = e if relative else e - position
            if not relative:
                position = e
            if delta <= 0:
                continue
            if "X" not in given and "Y" not in given:
                continue

            code = int(found.group(1))
            if code in (2, 3) and "I" in given and "J" in given:
                length = arc_length(before, at, given["I"], given["J"], code == 2)
            else:
                length = math.dist(before[:2], at[:2])
            out.moves.append(
                Move(
                    layer=layer,
                    region=region,
                    x=at[0],
                    y=at[1],
                    z=at[2],
                    from_z=before[2],
                    e=delta,
                    length=length,
                    raw=stripped,
                    stamped=STAMP in line,
                )
            )

    out.planes = planes
    out.layers = layer + 1
    return out


def arc_length(start, end, i: float, j: float, clockwise: bool) -> float:
    centre = (start[0] + i, start[1] + j)
    radius = math.hypot(i, j)
    if radius <= 0:
        return math.dist(start[:2], end[:2])
    first = math.atan2(start[1] - centre[1], start[0] - centre[0])
    last = math.atan2(end[1] - centre[1], end[0] - centre[0])
    sweep = first - last if clockwise else last - first
    if sweep <= 0:
        sweep += 2 * math.pi
    return radius * sweep


def surface(before: str, after: str) -> int:
    """What moved, by how much, and whether anything moved that must not.

    The two checks at the end only mean anything when the input is a file no
    transform has been over: bricking raises hidden walls of its own, and
    comparing against its output would report those as strays.
    """
    was, now = read(before), read(after)
    planes = was.planes
    bricked = "corbel brick" in open(before, errors="replace").read(1 << 22)

    touched = defaultdict(list)
    for move in now.moves:
        plane = planes.get(move.layer)
        if plane is None:
            continue
        rise = move.z - plane
        if abs(rise) > 1e-9:
            touched[move.region].append(rise)

    if not touched:
        print("nothing was followed")
        return 0

    print(f"{'region':<28}{'moves':>8}{'lowest':>10}{'highest':>10}{'mean':>10}")
    total = 0
    for region, rises in sorted(touched.items()):
        total += len(rises)
        print(
            f"{region:<28}{len(rises):>8}"
            f"{min(rises):>10.3f}{max(rises):>10.3f}"
            f"{sum(rises) / len(rises):>10.3f}"
        )
    layers = {move.layer for move in now.moves if abs(move.z - planes.get(move.layer, move.z)) > 1e-9}
    print(f"{'':<28}{total:>8} moves on {len(layers)} of {now.layers} layers")

    if bricked:
        print("the input was already bricked, so what else moved is not this transform's")
        return 0

    stray = [region for region in touched if not any(word in region for word in SURFACE)]
    if stray:
        print(f"FAIL: regions moved that are neither a surface nor the wall that shows: {sorted(stray)}")
        return 1

    kept = sum(1 for move in now.moves if not move.is_surface())
    original = sum(1 for move in was.moves if not move.is_surface())
    if kept != original:
        print(f"FAIL: {original} moves outside a surface became {kept}")
        return 1
    print(f"ok: {kept} moves outside a surface are untouched")
    return 0


def invariant(before: str, after: str) -> int:
    """Nothing may be commanded where a printer cannot put it.

    Both the plane and the layer height come from the input. Taking them from
    the output is exactly what this transform makes meaningless: a run that
    took every surface down half a layer would still look flat against its own
    minimum.
    """
    was, now = read(before), read(after)
    planes = was.planes
    heights = layer_heights(planes)
    bad = 0
    for move in now.moves:
        if not math.isfinite(move.z) or move.z < 0:
            print(f"FAIL: below the bed: {move.raw}")
            bad += 1
        if not math.isfinite(move.e) or move.e < 0:
            print(f"FAIL: negative extrusion: {move.raw}")
            bad += 1
    # A surface may never stand more than half a layer off its own plane: past
    # that it is printing into the layer above or the one below.
    for move in now.moves:
        height = heights.get(move.layer)
        plane = planes.get(move.layer)
        if height is None or plane is None:
            continue
        if abs(move.z - plane) > height / 2 + 1e-6:
            print(f"FAIL: {move.z - plane:+.3f} off a {height:.3f} layer: {move.raw}")
            bad += 1
    print(f"{'FAIL' if bad else 'ok'}: {bad} of {len(now.moves)} moves out of bounds")
    return 1 if bad else 0


def flow(before: str, after: str) -> int:
    """Every stretch has to be metered for the gap it really crosses.

    A stretch whose middle stands `d` above its plane crosses `height + d`
    where the slicer metered `height`, so its flow per mm has to be
    `(height + d) / height` of what it was. Matched by region and by total
    length rather than move for move, since one move of the input becomes
    several of the output.
    """
    was, now = read(before), read(after)
    planes = was.planes
    heights = layer_heights(planes)

    per_layer = defaultdict(lambda: [0.0, 0.0, 0.0])
    for move in was.moves:
        if move.is_surface() and move.layer in heights:
            per_layer[move.layer][0] += move.e
    for move in now.moves:
        if not move.is_surface():
            continue
        plane, height = planes.get(move.layer), heights.get(move.layer)
        if plane is None or height is None or move.length <= 0:
            continue
        per_layer[move.layer][1] += move.e
        # What the same path would have cost metered for its own gap.
        gap = height + move.middle() - plane
        per_layer[move.layer][2] += move.e * height / max(gap, 1e-9)

    worst = 0.0
    for layer, (sliced, written, undone) in sorted(per_layer.items()):
        if sliced <= 0:
            continue
        off = abs(undone - sliced) / sliced
        worst = max(worst, off)
    print(f"{'layer':>6}{'sliced':>10}{'written':>10}{'undone':>10}{'off':>8}")
    for layer, (sliced, written, undone) in sorted(per_layer.items()):
        if sliced <= 0 or abs(written - sliced) < 1e-9:
            continue
        off = abs(undone - sliced) / sliced
        print(f"{layer:>6}{sliced:>10.4f}{written:>10.4f}{undone:>10.4f}{off:>8.2%}")
    ok = worst < 0.02
    print(f"{'ok' if ok else 'FAIL'}: undoing the gap recovers what was sliced to {worst:.2%}")
    return 0 if ok else 1


def cover(path: str) -> int:
    """How wide each layer's exposed strip is, which is what can be followed.

    A strip is one tread of the staircase. Reported as the share of the layer's
    own extrusion that carries a region name a surface transform may touch.
    """
    read_in = read(path)
    per_layer = defaultdict(lambda: [0.0, 0.0])
    for move in read_in.moves:
        per_layer[move.layer][0] += move.length
        if move.is_surface():
            per_layer[move.layer][1] += move.length
    total = sum(value[0] for value in per_layer.values())
    exposed = sum(value[1] for value in per_layer.values())
    layers = sum(1 for value in per_layer.values() if value[1] > 0)
    print(f"{len(per_layer)} layers, {layers} of them with a surface region")
    print(f"{exposed:.0f} mm of surface out of {total:.0f} mm of extrusion "
          f"({0 if total == 0 else exposed / total:.2%})")
    return 0


def layer_heights(planes: dict[int, float]) -> dict[int, float]:
    heights = {}
    steps = sorted(planes.items())
    for index in range(1, len(steps)):
        layer, plane = steps[index]
        before = steps[index - 1][1]
        if plane > before:
            heights[layer] = plane - before
    return heights


def main(argv: list[str]) -> int:
    if len(argv) < 2:
        print(__doc__)
        return 2
    check = argv[1]
    if check == "surface" and len(argv) == 4:
        return surface(argv[2], argv[3])
    if check == "invariant" and len(argv) == 4:
        return invariant(argv[2], argv[3])
    if check == "flow" and len(argv) == 4:
        return flow(argv[2], argv[3])
    if check == "cover" and len(argv) == 3:
        return cover(argv[2])
    print(__doc__)
    return 2


if __name__ == "__main__":
    sys.exit(main(sys.argv))
