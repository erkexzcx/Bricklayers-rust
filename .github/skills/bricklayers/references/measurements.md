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

## The bed layer, and why a column climbs (2026-08-04)

Reported as "the letters under a Benchy fill in". Measured on
`benchy-maxwalls.gcode` (OrcaSlicer 2.4.2, 0.2/0.2, 240 layers).

The old rule raised the bed layer's internal loops by the full shift and
metered them at `(first_layer_height + shift) / first_layer_height` = 1.5.

| | before | after |
|---|---|---|
| layer 0 internal perimeter filament | 38.96 → 47.63 mm (+22.3%) | 38.96 mm (untouched) |
| layer 0 moves at 1.5× | 245 | 0 |
| highest factor anywhere, default settings | 1.5 | 1.25 |
| layer 0 vs the input | 8672 changed E words | **byte-identical** |
| raised loops | 1976 | 1958 |
| parity inversions | 22 | 21 |
| invariant (external perimeters raised) | 0 of 21832 | 0 of 21832 |

Why it showed up on the letters and not elsewhere:

- **The nameplate recess is exactly one layer deep.** 16 wall loops under 3 mm
  across sit inside the plate on layer 0 and **zero** on layer 1. So the whole
  of the visible text is formed by the layer that was being over-extruded.
- 12 of those loops were raised and given 1.5× flow, the smallest being
  0.32 × 0.57 mm with a 1.39 mm path — a letter counter. They carried only
  13.6% of layer 0's raised path, though; the full-width wall running along the
  letters carried the rest, so a "don't over-extrude thin beads" rule would not
  have fixed it.
- The 1.5× was arithmetically exact and rested on a false premise: that the
  bead becomes `first_layer_height + shift` tall. At 0.2 + 0.1 the nozzle sits
  0.3 above the plate with a 0.4 orifice, so it never touches the bead. The
  surplus widens it instead — 0.41 mm to 0.61 mm if the height stays at 0.2.

Not a filament-accounting bug: retract/prime pairs stayed balanced through the
change on four real files (benchy, orig-maxwalls, orig-2walls, 2dragons-2walls;
50k cycles, zero net drift), and geometry is move-for-move identical.

Sequential printing restarts the ramp per object, confirmed on
`2dragons-2walls.gcode`: layers 0 and 109 are entirely 1.0×, layers 1–2 and
110–111 are 1.25×, everything else 1.0×.

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

## What a bead is laid on (2026-08-13)

An inversion is not only a lost stagger — it is a **metering** event. Until this
was measured, the span a bead was fed for came from its own parity, so a loop
laid on the plane over a column that had been raised was fed for a whole layer
of gap when half of it was already filled.

`audit.py flow OUT IN` is the measurement: it matches each internal bead to the
column beneath it and compares `E_out/E_in` against `(h + rise − rise_below)/h ×
flow`. Stock OrcaSlicer 2.4.2 Benchy, 0.2 mm layers, 0.4 nozzle, arc fitting on:

| file | over-fed before | over-fed after | worst ratio |
|---|---|---|---|
| Benchy, 2 walls | 188 mm (0.88%) | **10 mm (0.04%)** | 2.00× |
| Benchy, 1000 walls | 2575 mm (3.23%) | **857 mm (1.08%)** | 2.00× |

The over-fed path is concentrated at **Z8–15 on the 2-wall file**, which is
where the hull flares hardest and the wall renumbers most often. Every ratio
seen is a clean fraction — 2.00 (flat over a raise, fed for a full layer), 0.67
(raised over the plane, fed for one layer where it spans one and a half), 0.50,
0.80, 0.83, 1.33 — so any new ratio is worth explaining before it is accepted.

The share of a loop's path standing on the layer below's raised footprint is
strongly bimodal, which is what lets `SEAM_SHARE` sit at 0.25:

| share of path over the raise below | loops on the plane | loops carrying a raise on |
|---|---|---|
| 0.0–0.1 | 789 / 798 (2 walls), 1848 / 2052 (1000) | 11 (2 walls), 112 (1000) |
| 0.4–1.0 | 5 (2 walls), 76 (1000) | 519 / 542 (2 walls), 1599 (1000) |

Sweeping the threshold on both files: 0.10 leaves 21.6% of the 1000-wall path
under-fed, 0.50 puts 2.8% of the 2-wall path back to over-fed. 0.20 and 0.25 are
indistinguishable on the 2-wall file; 0.25 is the better of the two on the
1000-wall one.

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
`; corbel brick raised` marker and cleared it on any other stamped line —
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
