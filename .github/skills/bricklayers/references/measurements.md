# Measurements

Every number here came from running the tool or an audit script over a real
sliced file. Nothing is estimated.

## Fixtures

Not in the repository — they are large. Downloaded from attachments on the
upstream tracker, plus one sliced locally.

| File | Slicer | Walls | Layers | Notes |
|---|---|---|---|---|
| `benchy-orig.gcode` | OrcaSlicer | 1000 | 240 | arc fitting on, `; FEATURE:` markers, 0.2/0.2 heights |
| `benchy_2perim_raw.gcode` | PrusaSlicer 2.9.0 | 2 | 240 | one internal loop per wall, raised against the external perimeter |
| `shapebox_vanilla.gcode` | PrusaSlicer 2.9.0 | 2 | 194 | every layer has exactly one internal loop; first layer 0.3 vs 0.2 |
| `water_test.gcode` | OrcaSlicer 2.1.1 | 7 | 63 | first layer 0.6 vs layer 0.8; the elephant-foot report |
| `pump_housing.gcode` | PrusaSlicer 2.7.4 | 3 | 247 | already Python-processed, geometry still valid |
| `wheel_scarf_off.gcode` | OrcaSlicer 2.2.0 | 6 | 30 | no arcs at all, good contrast case |

Two binary fixtures live in `tests/fixtures/` (from prusa3d/libbgcode). Both are
two-perimeter mini cubes, which raise their single internal loop per contour
since the `loops > 1` guard was removed; the binary round-trip test still
repacks synthetic multi-loop G-code through the real container, because it was
written when those fixtures raised nothing.

## Contour signals

Consecutive loop pairs inside internal perimeter regions, by minimum distance
between the two paths:

| File | ≤0.75 mm | 0.75–2 mm | >2 mm |
|---|---|---|---|
| benchy-orig | 2330 | 67 | 122 |
| wheel_scarf_off | 2022 | 31 | 711 |
| pump_housing | 583 | 76 | 1290 |
| water_test | 0 | 40 | 0 |

Bimodal in every file. water_test sits higher because its extrusion width is
1.26 mm.

The same pairs measured by distance from one loop's end to the next one's
start — the signal that looks equivalent and is not:

| Distance | Pairs (benchy-orig) |
|---|---|
| ≤0.5 mm | 192 |
| 0.5–1 mm | 544 |
| 1–3 mm | 530 |
| 3–10 mm | 605 |
| >10 mm | 647 |

Flat. Unusable.

## Raised-loop counts

How each grouping rule performed. Same input files throughout.

| File | Retraction rule | Bbox nesting | Adjacency (current) |
|---|---|---|---|
| benchy-orig (1000 walls) | — | 314 / 3271 | **1413 / 3277** |
| benchy_2perim | 1026 / 1960 | 4 / 1960 | **773 / 1960** |
| pump_housing | — | 171 / 2922 | **650 / 2922** |
| wheel_scarf_off | — | 1555 / 3961 | **1618 / 3961** |
| water_test | 24 / 213 | 24 / 159 | **24 / 159** |
| shapebox | 0 / 194 | 0 / 194 | **0 / 194** |

water_test's loop count fell from 213 to 159 when the region was reset at layer
changes: 54 of the old "loops" were stray segments emitted before the next
`;TYPE:` marker.

shapebox raises nothing and that is correct — 194 layers, one internal loop
each, nothing to stagger against.

## Layer-to-layer parity

Inversions of the outermost hull loop on benchy-orig, over the 155 layers that
have a multi-loop wall:

| Numbering anchor | Inversions | Layers with the outermost loop raised |
|---|---|---|
| first loop printed | 23 | 107 / 155 |
| **loop against the external perimeter** | **6** | **152 / 155** |

22 of the 23 inversions landed exactly on a layer where the hull's loop count
changed (19 → 20 → 21 → 22 …). The remaining 6 are the 9 layers OrcaSlicer
chooses to print outer-first.

## Arcs

benchy-orig, by region:

| Region | Linear | Arcs | Share |
|---|---|---|---|
| internal perimeter | 19089 | 8954 | 32% |
| external perimeter | 20783 | 1569 | 7% |
| other | 10730 | 1113 | 9% |

Before arcs counted as extrusions: 321 of 3271 loops opened with an arc,
stranding 140 arc moves outside the loop they belong to. After: 0.

Six loops on that file are drawn *entirely* as arcs and were invisible to the
transform altogether.

## Safety invariant

No external perimeter may be extruded while the nozzle is raised. Across six
processed files: **0 of 160 017** external perimeter extrusions.

| File | External extrusions | Raised |
|---|---|---|
| benchy-orig (processed) | 21 832 | 0 |
| benchy_2perim | 27 837 | 0 |
| pump_housing | 50 896 | 0 |
| wheel_scarf_off | 37 612 | 0 |
| water_test | 20 870 | 0 |
| shapebox | 970 | 0 |

This has held through every rewrite of the grouping rule and is the check that
matters most.

**The first version of this check was worthless.** It tracked the raise from the
`; bricklayers brick raised` marker and cleared it on any other stamped line —
including the `resume` line that immediately follows every raise. It therefore
had a window of one line and would have reported 0 on anything.
`scripts/audit.py` now compares the nozzle Z against the height the file itself
last commanded, and is verified against a positive control that must report
exactly one violation.

## Parity baseline

From `scripts/audit.py parity`, which measures every internal region rather than
one per layer:

| File | Regions with a multi-loop wall | Outermost raised | Inversions |
|---|---|---|---|
| benchy-orig (processed) | 276 | 256 | 8 |

Six of the eight land on a region where the loop count also changed.

## Performance

The 1000-wall Benchy (3.1 MB) goes through `brick` in 21 ms. Memory is flat at
~14 MB regardless of input size, because only the current region is buffered.
