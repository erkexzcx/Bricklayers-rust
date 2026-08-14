"""Shared G-code reading for corbel's audit scripts.

Mirrors what src/brick.rs does, so a measurement taken here says something
about the transform rather than about this parser. In particular: regions end
at a layer change as well as at a region marker, arcs count as extrusion, and
loops are grouped into contours by whether their paths run beside each other.
"""

from __future__ import annotations

import math
import re
from dataclasses import dataclass, field

# Matching src/brick.rs.
MAX_LOOP_GAP = 2.0
PROBES = 16

EXTERNAL = ("external perimeter", "outer wall", "wall-outer")
INTERNAL = ("inner wall", "wall-inner", "perimeter")
SOLID = ("solid", "top surface", "bottom surface", "bridge", "skin")

WORD = re.compile(r"(?:^|\s)([XYZEF])(-?\d*\.?\d+)")


def classify(label: str) -> str:
    """Region kind, matching src/gcode/feature.rs. Order matters."""
    low = label.strip().lower()
    # Before the wall tests: "Overhang perimeter" carries both words. An
    # overhang is labelled in place of the wall it belongs to and names no
    # wall of its own -- slicers interrupt an inner wall with it mid-loop --
    # so counting it as external made every raised inner wall look like a
    # violation. Checking only the beads a slicer really called the outer
    # wall keeps the invariant's teeth: a raised visible loop carries that
    # label on at least part of itself, and the whole loop shares one height.
    if "overhang" in low:
        return "overhang"
    if any(needle in low for needle in EXTERNAL):
        return "external"
    if any(needle in low for needle in INTERNAL):
        return "internal"
    if any(needle in low for needle in SOLID):
        return "solid"
    if "infill" in low or "fill" in low:
        return "sparse"
    return "other"


def marker(line: str) -> str | None:
    """Region kind of a `;TYPE:` or `; FEATURE:` comment, else None.

    OrcaSlicer emits `; FEATURE: Inner wall` on some flavours, so a file can
    contain no `;TYPE:` at all.
    """
    text = line.strip()
    if not text.startswith(";"):
        return None
    text = text[1:].lstrip()
    for key in ("TYPE:", "FEATURE:"):
        if text[: len(key)].upper() == key:
            return classify(text[len(key) :])
    return None


def is_layer_change(line: str) -> bool:
    text = line.strip()
    if not text.startswith(";"):
        return False
    text = text[1:].lstrip().upper()
    return (
        text == "LAYER_CHANGE"
        or text == "CHANGE_LAYER"
        or (text[:6] == "LAYER:" and len(text) > 6)
    )


def words(body: str) -> dict[str, float]:
    return {letter: float(value) for letter, value in WORD.findall(body)}


@dataclass
class Loop:
    """One perimeter loop: the points it extrudes, and how it was drawn."""

    points: list[tuple[float, float]] = field(default_factory=list)
    arcs: int = 0
    linear: int = 0
    raised: bool = False

    @property
    def box(self) -> tuple[float, float, float, float]:
        xs = [p[0] for p in self.points]
        ys = [p[1] for p in self.points]
        return (min(xs), min(ys), max(xs), max(ys))

    @property
    def size(self) -> float:
        left, bottom, right, top = self.box
        return (right - left) * (top - bottom)


def adjacent(previous: Loop, current: Loop, gap: float = MAX_LOOP_GAP) -> bool:
    """True when the two loops run within `gap` of each other anywhere.

    Bounding boxes reject most pairs outright; the rest are probed.
    """
    a, b = previous.box, current.box
    apart = max(a[0] - b[2], b[0] - a[2], a[1] - b[3], b[1] - a[3])
    if apart > gap:
        return False
    stride = max(1, -(-len(current.points) // PROBES))
    limit = gap * gap
    for x, y in current.points[::stride]:
        for px, py in previous.points:
            if (px - x) ** 2 + (py - y) ** 2 <= limit:
                return True
    return False


def nearest(previous: Loop, current: Loop) -> float:
    """Minimum distance between the two loops' points."""
    best = math.inf
    for x, y in current.points:
        for px, py in previous.points:
            best = min(best, math.hypot(px - x, py - y))
    return best


def contours(loops: list[Loop], gap: float = MAX_LOOP_GAP) -> list[list[Loop]]:
    if not loops:
        return []
    groups = [[loops[0]]]
    for current in loops[1:]:
        if adjacent(groups[-1][-1], current, gap):
            groups[-1].append(current)
        else:
            groups.append([current])
    return groups


def planes(path: str) -> list[float | None]:
    """The height each layer is printed at, as the lowest Z commanded in it.

    A Z-hop and a raise both only ever lift the nozzle, so a layer's floor is
    the layer. Taking the plane from the last un-stamped `G1 Z` instead breaks
    as soon as a height change rides one: the move this tool writes a raise
    onto is often the slicer's own hop restore, which is where the layer's
    height was being read from.
    """
    found: list[float | None] = []
    lowest = None
    for raw in open(path, errors="replace"):
        line = raw.strip()
        if is_layer_change(line):
            found.append(lowest)
            lowest = None
            continue
        body = line.split(";")[0].strip()
        # Arcs carry Z only for helical lifts, which are not the plane.
        if body[:2] not in ("G0", "G1"):
            continue
        z = words(body[2:]).get("Z")
        if z is not None and (lowest is None or z < lowest):
            lowest = z
    found.append(lowest)
    return found


def regions(path: str, kind: str = "internal"):
    """Yields `(layer, loops)` for every region of `kind` in the file.

    A region ends at the next region marker *or* at a layer change. Missing
    the layer change merges the stray segment slicers emit before re-declaring
    a region into the previous layer, which has produced false findings before.

    A loop counts as raised when the nozzle sits above its layer's own plane.
    Do NOT read that off the stamps: `reset` is only emitted when this tool
    moves Z back down itself, so a loop raised at the end of a layer stays
    "raised" through the slicer's own layer-change move and every loop after it
    is mislabelled. Measured on a real 2-wall slice, that leak reported 26
    layers as having flipped when not one commanded Z had changed.
    """
    layer = 0
    feature = "other"
    nozzle_z = None
    floors = planes(path)
    loops: list[Loop] = []
    travelled = True

    def flush():
        nonlocal loops
        ready, loops = loops, []
        return ready

    for raw in open(path, errors="replace"):
        line = raw.strip()
        if is_layer_change(line):
            if feature == kind and loops:
                yield layer, flush()
            loops = []
            layer += 1
            feature = "other"
            travelled = True
            continue
        found = marker(line)
        if found is not None:
            if feature == kind and loops:
                yield layer, flush()
            loops = []
            feature = found
            travelled = True
            continue
        ours = "corbel" in line
        body = line.split(";")[0].strip()
        arc = body[:2] in ("G2", "G3")
        linear = body[:2] in ("G0", "G1")
        if not arc and not linear:
            continue
        found_words = words(body[2:])
        # Tracked outside the region too: the layer's own Z move lands before
        # the region is re-declared.
        if "Z" in found_words:
            nozzle_z = found_words["Z"]
        has_xy = "X" in found_words and "Y" in found_words
        # A travel is a travel whether or not this tool stamped a height onto
        # it. Height changes ride the travel that reaches a loop, so skipping
        # a stamped line outright merges the loops on either side of it and
        # every loop after the first reads as part of its neighbour.
        if ours:
            if linear and has_xy:
                travelled = True
            continue
        if feature != kind:
            continue
        extrusion = found_words.get("E")
        # Arcs extrude too, and a loop that opens with one still opens there.
        if has_xy and extrusion is not None and extrusion > 0:
            if not loops or travelled:
                floor = floors[layer] if layer < len(floors) else None
                raised = (
                    nozzle_z is not None
                    and floor is not None
                    and nozzle_z > floor + 1e-6
                )
                loops.append(Loop(raised=raised))
            loops[-1].points.append((found_words["X"], found_words["Y"]))
            if arc:
                loops[-1].arcs += 1
            else:
                loops[-1].linear += 1
            travelled = False
        elif linear and has_xy:
            travelled = True

    if feature == kind and loops:
        yield layer, loops
