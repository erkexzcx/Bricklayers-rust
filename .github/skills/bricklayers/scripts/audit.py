#!/usr/bin/env python3
"""Audit sliced or post-processed G-code for corbel's BrickLayers transform.

    python3 audit.py invariant  part.gcode [original.gcode]
    python3 audit.py flow       part.gcode original.gcode  # metered for the real gap?
    python3 audit.py parity     part.gcode   # layer-to-layer stagger inversions
    python3 audit.py contours   part.gcode   # how loops group, and why groups end
    python3 audit.py adjacency  part.gcode   # distance between consecutive loops
    python3 audit.py arcs       part.gcode   # share of extrusion drawn as G2/G3
    python3 audit.py all        part.gcode

`invariant` is the one that must always pass. The rest are measurements: read
them against .github/skills/bricklayers/references/measurements.md rather than
against intuition.

Pass the original file too whenever the surface transform has also run over it:
its output deliberately no longer sits on one height per layer, so a plane read
back out of it is not the plane the slicer chose.
"""

from __future__ import annotations

import math
import re
import sys
from collections import Counter

sys.path.insert(0, __file__.rsplit("/", 1)[0])

from gcode import (  # noqa: E402
    contours,
    is_layer_change,
    marker,
    nearest,
    planes,
    regions,
    words,
)

MOVE = re.compile(r"^G[0123](?=\s)")


def invariant(path: str, source: str | None = None) -> bool:
    """No external perimeter may be extruded while the nozzle is raised.

    That is the defect the upstream project is best known for. It has never
    reproduced here, and any change that breaks it is wrong.

    The nozzle is raised whenever it sits above the height the file itself last
    commanded. Do NOT decide this from the markers alone: a raise is followed
    by a `resume` line that carries the stamp but says nothing about Z, and the
    slicer's own Z moves bring the nozzle down without any marker at all.

    Where the surface transform has also run, the file no longer sits on one
    height per layer and the last height it commanded is not the plane. Pass
    `source` — the file this one came from — and each layer's plane is taken
    from there instead.
    """
    feature = "other"
    relative = True
    previous_e = None
    nozzle_z = None
    layer_z = None
    # `planes` files everything before the first layer marker at index
    # zero, so the first printed layer is index one.
    layer = 0
    floors = planes(source) if source else []
    total = 0
    hits = []

    for number, raw in enumerate(open(path, errors="replace"), 1):
        line = raw.strip()
        if is_layer_change(line):
            layer += 1
        found = marker(line)
        if found is not None:
            feature = found
            continue
        if line.startswith("M82"):
            relative = False
        elif line.startswith("M83"):
            relative = True
        if not MOVE.match(line):
            continue

        ours = "corbel" in line
        body = line.split(";")[0]
        found_words = words(body[2:])
        if "Z" in found_words:
            nozzle_z = found_words["Z"]
            if not ours:
                layer_z = nozzle_z
        if floors:
            layer_z = floors[layer] if 0 <= layer < len(floors) else None

        # A line this tool wrote is still an extrusion if it lays a bead: the
        # surface transform stamps the moves it rewrites, and those carry an
        # `E` word. Skipping them would leave the visible wall's own beads
        # unchecked, and in absolute mode would lose track of the position too.
        extrusion = found_words.get("E")
        if extrusion is None:
            continue
        if relative:
            extruding = extrusion > 1e-9
        else:
            extruding = previous_e is not None and extrusion > previous_e + 1e-9
            previous_e = extrusion
        if not extruding or not ("X" in found_words or "Y" in found_words):
            continue
        if feature == "external":
            total += 1
            if nozzle_z is not None and layer_z is not None and nozzle_z > layer_z + 1e-6:
                hits.append((number, line[:74]))

    print(f"  external perimeter extrusions : {total}")
    print(f"  ... emitted while raised      : {len(hits)}")
    for number, text in hits[:5]:
        print(f"      line {number}: {text}")
    return not hits


def parity(path: str) -> bool:
    """Does the same loop stay raised from layer to layer?

    The stagger is sideways. Two layers looking alike is correct; a layer where
    the pattern inverts breaks the raised column.
    """
    sequence = []
    counts = []
    for layer, loops in regions(path):
        groups = [group for group in contours(loops) if len(group) >= 2]
        if not groups:
            continue
        wall = max(groups, key=lambda group: group[0].size)
        outermost = max(wall, key=lambda loop: loop.size)
        sequence.append("R" if outermost.raised else "-")
        counts.append((layer, len(wall)))

    if not sequence:
        print("  no multi-loop walls found; nothing to brick")
        return True

    flips = sum(1 for a, b in zip(sequence, sequence[1:]) if a != b)
    with_count_change = sum(
        1
        for (a, b), (_, size), (_, next_size) in zip(
            zip(sequence, sequence[1:]), counts, counts[1:]
        )
        if a != b and size != next_size
    )
    raised = sequence.count("R")
    print(f"  layers with a multi-loop wall : {len(sequence)}")
    print(f"  outermost loop raised         : {raised}")
    print(f"  layer-to-layer inversions     : {flips}")
    print(f"  ... where the loop count also changed: {with_count_change}")
    for at in range(0, min(len(sequence), 180), 60):
        print("      " + "".join(sequence[at : at + 60]))
    return True


def contour_census(path: str) -> bool:
    sizes = Counter()
    endings = Counter()
    for _, loops in regions(path):
        groups = contours(loops)
        for group in groups:
            sizes[len(group)] += 1
        for index in range(1, len(loops)):
            if any(loops[index] is group[0] for group in groups[1:]):
                gap = nearest(loops[index - 1], loops[index])
                endings["a travel away (>10 mm)" if gap > 10 else f"{gap:.0f}-{gap + 1:.0f} mm away"] += 1

    total = sum(count * size for size, count in sizes.items())
    lone = sizes.get(1, 0)
    print(f"  loops                         : {total}")
    print(f"  contours                      : {sum(sizes.values())}")
    print(f"  ... holding a single loop     : {lone}")
    for size, count in sorted(sizes.items())[:8]:
        print(f"      {count:6d} contours of {size} loop(s)")
    print("  why a contour ended:")
    for reason, count in endings.most_common(6):
        print(f"      {count:6d}  {reason}")
    return True


def adjacency(path: str) -> bool:
    """Distance between consecutive loops, which is what groups them.

    Expect two clusters: one extrusion width, and a travel. Anything in
    between means the threshold needs looking at.
    """
    near = Counter()
    hops = Counter()

    def bucket(value: float) -> str:
        for edge in (0.25, 0.5, 0.75, 1.0, 1.5, 2.0, 3.0, 5.0, 10.0):
            if value <= edge:
                return f"<={edge}"
        return ">10"

    for _, loops in regions(path):
        for index in range(1, len(loops)):
            before, current = loops[index - 1], loops[index]
            near[bucket(nearest(before, current))] += 1
            hops[
                bucket(
                    math.hypot(
                        before.points[-1][0] - current.points[0][0],
                        before.points[-1][1] - current.points[0][1],
                    )
                )
            ] += 1

    order = ["<=0.25", "<=0.5", "<=0.75", "<=1.0", "<=1.5", "<=2.0", "<=3.0", "<=5.0", "<=10.0", ">10"]
    print("  minimum distance between the two paths (this is the signal):")
    for key in order:
        if near[key]:
            print(f"      {near[key]:6d}  {key} mm")
    print("  end-of-loop to start-of-next (this one is noise, do not use it):")
    for key in order:
        if hops[key]:
            print(f"      {hops[key]:6d}  {key} mm")
    return True


def arcs(path: str) -> bool:
    linear = Counter()
    curved = Counter()
    feature = "other"
    for raw in open(path, errors="replace"):
        line = raw.strip()
        if is_layer_change(line):
            feature = "other"
            continue
        found = marker(line)
        if found is not None:
            feature = found
            continue
        if re.match(r"^G[01] .*E[0-9.]", line):
            linear[feature] += 1
        elif re.match(r"^G[23] .*E[0-9.]", line):
            curved[feature] += 1

    opened_with_arc = 0
    for _, loops in regions(path):
        opened_with_arc += sum(1 for loop in loops if loop.linear == 0 and loop.arcs)
    for kind in sorted(set(linear) | set(curved), key=lambda k: -(linear[k] + curved[k])):
        total = linear[kind] + curved[kind]
        share = 100 * curved[kind] / total if total else 0
        print(f"      {kind:10s} linear {linear[kind]:7d}  arcs {curved[kind]:7d}  {share:4.0f}%")
    print(f"  wall loops drawn only as arcs : {opened_with_arc}")
    return True


def flow(path: str, source: str) -> bool:
    """Is every internal bead metered for the gap it really crosses?

    The gap a bead has to fill is `h + rise(here) - rise(what it is laid on)`,
    and the tool's own answer is its `E` over the same bead's `E` in the file
    it came from. Where the two disagree the bead is over- or under-fed by
    exactly that ratio, and over-fed is the direction that blobs: a bead laid
    on the plane over a raise it was not told about carries **twice** what
    fits.

    The column below is matched geometrically, and refused wherever the match
    is ambiguous — neighbouring loops run about 0.41 mm apart and a column on a
    slope shifts sideways as it climbs, so the nearest bead below can belong to
    the loop beside this one. Anything within the search radius that disagrees
    about its height makes the answer unusable.

    A measurement, not a pass/fail: metering is decided per loop while the gap
    varies along it, so a part whose walls wander over mixed ground keeps a few
    percent. Read it against the same file before the change.
    """
    grid = 0.30
    floors = planes(source)
    before, after = _beads(source), _beads(path)
    if len(before) != len(after):
        print(f"  bead counts differ: {len(before)} in, {len(after)} out")
        print("  (the surface transform splits moves; compare a bricks-only run)")
        return True

    height = _stated(source, "layer_height") or 0.2
    per_layer: dict[int, list] = {}
    for was, now in zip(before, after):
        layer, kind, x0, y0, x1, y1, z, e = now
        if kind not in ("internal", "overhang") or x0 is None:
            continue
        if layer >= len(floors) or floors[layer] is None or z is None:
            continue
        mid = ((x0 + x1) / 2.0, (y0 + y1) / 2.0)
        run = math.dist((x0, y0), (x1, y1))
        per_layer.setdefault(layer, []).append(
            (mid, round(z - floors[layer], 4), was[7], e, run)
        )

    buckets = {}
    for layer, items in per_layer.items():
        cells: dict[tuple[int, int], list] = {}
        for mid, rise, *_ in items:
            key = (int(mid[0] / grid), int(mid[1] / grid))
            cells.setdefault(key, []).append((mid, rise))
        buckets[layer] = cells

    def under(layer: int, mid: tuple[float, float]):
        cells = buckets.get(layer - 1)
        if not cells:
            return None
        cx, cy = int(mid[0] / grid), int(mid[1] / grid)
        rises = set()
        for dx in (-1, 0, 1):
            for dy in (-1, 0, 1):
                for other, rise in cells.get((cx + dx, cy + dy), ()):
                    if math.dist(mid, other) < grid:
                        rises.add(rise)
        return rises.pop() if len(rises) == 1 else None

    # The flow the walls are metered at, read off the file rather than assumed:
    # the commonest ratio among beads whose gap is exactly one layer.
    ratios: Counter = Counter()
    checked = []
    for layer in sorted(per_layer):
        if layer < 2:
            continue
        for mid, rise, was_e, now_e, run in per_layer[layer]:
            below = under(layer, mid)
            if below is None or run < 0.1 or not was_e:
                continue
            checked.append((rise, below, was_e, now_e, run))
            if rise == below:
                ratios[round(now_e / was_e, 3)] += 1
    wall = ratios.most_common(1)[0][0] if ratios else 1.0

    total = sum(run for *_, run in checked)
    high = low = 0.0
    worst = Counter()
    for rise, below, was_e, now_e, run in checked:
        want = (height + rise - below) / height * wall
        got = now_e / was_e
        if abs(got - want) > 0.02 * want:
            worst[round(got / want, 2)] += run
            if got > want:
                high += run
            else:
                low += run
    print(f"  wall flow read off the file  : {wall}")
    print(f"  matched internal path        : {total:.0f} mm")
    print(f"  ... over-fed                 : {high:.0f} mm ({_share(high, total)})")
    print(f"  ... under-fed                : {low:.0f} mm ({_share(low, total)})")
    for ratio, run in worst.most_common(6):
        print(f"      at {ratio:5.2f}x what the gap holds : {run:8.1f} mm")
    return True


def _share(part: float, whole: float) -> str:
    return f"{100 * part / whole:.2f}%" if whole else "n/a"


def _stated(path: str, key: str) -> float | None:
    """A `; key = value` setting out of a slicer's own config block."""
    want = re.compile(rf"^;\s*{key}\s*=\s*([0-9.]+)\s*$")
    for raw in open(path, errors="replace"):
        found = want.match(raw.strip())
        if found:
            return float(found.group(1))
    return None


def _beads(path: str) -> list:
    """Every extruding XY move, in order, with where and how high it was laid."""
    layer = 0
    kind = "other"
    x = y = z = None
    found = []
    for raw in open(path, errors="replace"):
        line = raw.strip()
        if is_layer_change(line):
            layer, kind = layer + 1, "other"
            continue
        seen = marker(line)
        if seen is not None:
            kind = seen
            continue
        body = line.split(";")[0].strip()
        if body[:2] not in ("G0", "G1", "G2", "G3"):
            continue
        w = words(body[2:])
        z = w.get("Z", z)
        nx, ny, e = w.get("X", x), w.get("Y", y), w.get("E")
        if nx is not None and ny is not None and e is not None and e > 0:
            found.append((layer, kind, x, y, nx, ny, z, e))
        x, y = nx, ny
    return found


CHECKS = {
    "invariant": invariant,
    "flow": flow,
    "parity": parity,
    "contours": contour_census,
    "adjacency": adjacency,
    "arcs": arcs,
}

# Checks that compare the file against the one it came from rather than
# measuring it on its own.
PAIRED = ("invariant", "flow")


def main(argv: list[str]) -> int:
    if len(argv) not in (3, 4) or (argv[1] not in CHECKS and argv[1] != "all"):
        print(__doc__)
        return 2
    check, path = argv[1], argv[2]
    source = argv[3] if len(argv) == 4 else None
    if check == "flow" and not source:
        print(__doc__)
        return 2
    chosen = CHECKS if check == "all" else {check: CHECKS[check]}
    ok = True
    for name, run in chosen.items():
        if name == "flow" and not source:
            continue
        print(f"=== {name}")
        ok &= bool(run(path, source) if name in PAIRED and source else run(path))
    return 0 if ok else 1


if __name__ == "__main__":
    raise SystemExit(main(sys.argv))
