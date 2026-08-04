#!/usr/bin/env python3
"""Audit sliced or post-processed G-code for the bricklayers transform.

    python3 audit.py invariant  part.gcode   # no external wall extruded raised
    python3 audit.py parity     part.gcode   # layer-to-layer stagger inversions
    python3 audit.py contours   part.gcode   # how loops group, and why groups end
    python3 audit.py adjacency  part.gcode   # distance between consecutive loops
    python3 audit.py arcs       part.gcode   # share of extrusion drawn as G2/G3
    python3 audit.py all        part.gcode

`invariant` is the one that must always pass. The rest are measurements: read
them against .github/skills/bricklayers/references/measurements.md rather than
against intuition.
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
    regions,
    words,
)

MOVE = re.compile(r"^G[0123](?=\s)")


def invariant(path: str) -> bool:
    """No external perimeter may be extruded while the nozzle is raised.

    That is the defect the upstream project is best known for. It has never
    reproduced here, and any change that breaks it is wrong.

    The nozzle is raised whenever it sits above the height the file itself last
    commanded. Do NOT decide this from the markers alone: a raise is followed
    by a `resume` line that carries the stamp but says nothing about Z, and the
    slicer's own Z moves bring the nozzle down without any marker at all.
    """
    feature = "other"
    relative = True
    previous_e = None
    nozzle_z = None
    layer_z = None
    total = 0
    hits = []

    for number, raw in enumerate(open(path, errors="replace"), 1):
        line = raw.strip()
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

        ours = "bricklayers" in line
        body = line.split(";")[0]
        found_words = words(body[2:])
        if "Z" in found_words:
            nozzle_z = found_words["Z"]
            if not ours:
                layer_z = nozzle_z
        if ours:
            continue

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


CHECKS = {
    "invariant": invariant,
    "parity": parity,
    "contours": contour_census,
    "adjacency": adjacency,
    "arcs": arcs,
}


def main(argv: list[str]) -> int:
    if len(argv) != 3 or (argv[1] not in CHECKS and argv[1] != "all"):
        print(__doc__)
        return 2
    check, path = argv[1], argv[2]
    chosen = CHECKS if check == "all" else {check: CHECKS[check]}
    ok = True
    for name, run in chosen.items():
        print(f"=== {name}")
        ok &= bool(run(path))
    return 0 if ok else 1


if __name__ == "__main__":
    raise SystemExit(main(sys.argv))
