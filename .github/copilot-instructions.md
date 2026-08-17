# corbel — project guidelines

A single Rust binary that post-processes sliced G-code with two independent
transforms: **BrickLayers** (`--bricks`), which raises alternate internal
perimeter loops by half a layer height, and **Z anti-aliasing** (`--zaa`), which
follows the model's surface inside a layer so a shallow top ramps instead of
stepping. Either, or both. It takes no sub-command, only a file plus a handful
of flags, and **a run naming neither transform is refused with a non-zero exit
code** rather than given a default — it runs as a slicer post-processing script,
so it is handed the user's only copy of a file and must never apply a transform
nobody asked for.

## Fix it, do not report it

**Anything you find is either a bug or a limitation. There is no third
category, and "worth doing later" is not one.**

- If it can genuinely be fixed, **fix it, without asking**. Large is not an
  excuse to defer: if the fix is a week of work, do the week of work.
- Only something that truly cannot be fixed is a limitation, and a limitation
  goes in README.md with the measurement behind it.
- Never end a turn offering to fix a defect you have already found. Fix it,
  verify it, then say what changed.
- A defect found in someone's real file is worth more than any synthetic test.
  Reproduce it, fix it, and add the test the fixture would have needed.

## Read this first

**Before changing `src/brick.rs`, `src/scan.rs` or `src/gcode/feature.rs`, load
[.github/skills/bricklayers/SKILL.md](skills/bricklayers/SKILL.md).** It holds the
geometric model, the contour-grouping rules with the measurements behind them,
the signals in sliced G-code that look useful and are traps, and
`scripts/audit.py` for verifying output against a real file. Every wrong turn
recorded there started as a confident claim about what a slicer emits.

**Before changing `src/zaa.rs`, `src/zaa/surface.rs` or `src/zaa/scout.rs`, load
[.github/skills/zaa/SKILL.md](skills/zaa/SKILL.md).** It holds how a layer's
surface is recovered from the outlines either side of it without the model, which
regions are reshaped and which are left where the slicer put them, the real-file
measurements, and its own `scripts/audit.py`. The most expensive wrong turn there
was assuming a top-surface region covers the exposed strip — on a real slice it
barely exists, because the walls cover it.

**Do not assert what a slicer emits — measure it.** Slice a real file, count,
then write the code. Numbers that go into the source or the skill must come from
a real print, not from reading slicer source or reasoning from first principles.

**The README's diagrams are generated, not drawn.** There is one per transform, both in
`img/`. Before restyling one or changing a constant a picture depends on, load
[.github/skills/diagrams/SKILL.md](skills/diagrams/SKILL.md); `scripts/pin.py`
there proves the figures still agree with `src/brick.rs`, `src/zaa/surface.rs` and
with the compiled binary, and it has to pass before `scripts/render.py` and
`scripts/contour.py` are run. A figure carries text, so a rename reaches it.

## Build and test

**Every change must be covered by a test. This is not negotiable.** A change that
adds behaviour adds a test that fails without it; a change that is meant to keep
behaviour identical adds a test that pins the old behaviour, and where the output
is a whole file, diff it against a build from before the change and say so. No
change is done because it compiles and the existing suite is green — the existing
suite did not know about it.

Everything below must pass before a change is done:

```sh
cargo fmt --all
cargo clippy --all-targets -- -D warnings
cargo test
```

`cargo test` runs the unit tests plus `tests/end_to_end.rs`, which drives the real
compiled binary against a synthetic PrusaSlicer file and asserts the output is
still coherent G-code, and `tests/binary_gcode.rs`, which pins the decoder against
Prusa's own `libbgcode` test files.

`cargo bench` runs `benches/throughput.rs`: no framework, just a synthetic slice
and a wall clock over survey and `brick`. Run it on both sides of a change that
is meant to be faster, and put the numbers in the commit message.

Fixtures are not enough — check a change against a real slice too. `--output`
leaves the input intact and `--verbose` says whether anything happened:

```sh
cargo run -- --bricks --zaa --verbose --output /tmp/out.gcode ~/Downloads/part.gcode
# corbel: 90 layers, 180 perimeter loops, 53 raised by 0.100 mm
# corbel: 4928.6 mm filament, 6.2% of it in raised loops; a flow of 1.025 adds 0.45% to the part
# corbel: 1168 surface moves on 86 layers followed from -0.080 to +0.100 mm of their plane, written as 3041 moves
```

Zero counts mean the region markers were not recognised — grep the input for
`;TYPE:` *and* `; FEATURE:`. Then `grep -n corbel /tmp/out.gcode` for the
inserted Z moves, and run both skills' `scripts/audit.py` over the result.

A Benchy is a poor subject for the surface transform — it is mostly steep. Slice a
shallow spherical cap instead; the zaa skill has the headless OrcaSlicer command.

## Architecture

Two passes over the input, and the file is never held in memory: `Source::open`
→ `survey()` (counters only) → `sink()` → `rewrite()` (`BufRead` in, `Write` out).
The surface transform adds a third read of its own — see `src/zaa/scout.rs`. Peak RSS
is flat at ~14 MiB for bricking, whatever the file's size, bounded by one layer
rather than by the file. The surface transform costs its grid on top of that,
and the grid is a fixed budget of cells rather than a fixed resolution
(`surface::MAX_WINDOW`), so it too is bounded by neither the file nor the part:
measured 20.0 MiB with both transforms on a Benchy and 56.5 MiB on a 22.8 MB
180 mm dome, where only the resolution differs between them.

The two transforms compose in **one pass**: `zaa::Pass` is a `Write` that sits in
front of the real one, so `brick::stream` writes into it and it writes the file.
They own different regions, so neither sees the other's work.

The tree is one file per subsystem, with a directory beside it where that
subsystem needs more than one. A test module big enough to bury the code it
tests lives in `tests.rs` beside it — still a `#[cfg(test)] mod tests`, just not
in the way.

| file | |
|---|---|
| `src/gcode.rs` | byte-scanner line parser (no regex), `Extruder` M82/M83 mapping, `Modal` G90/G91/G20/G21 tracking, Marlin checksums, `Lines` reader |
| `src/gcode/feature.rs` | Prusa/Orca/Bambu/Cura region markers → one enum |
| `src/geometry.rs` | plane geometry both transforms share, re-exported flat |
| `src/geometry/footprint.rs` | where a layer's material sits, as grid cells, so "is anything above this?" is a binary search; `Grid` picks how fine those cells are, and `Trace` says whether a walk could be finished at all |
| `src/geometry/inset.rs` | moving a closed loop sideways, toward the material behind it |
| `src/scan.rs` | `Survey`: the single pre-pass, and `Markerless`, the layer layout for a file that states none |
| `src/brick.rs`, `src/brick/tests.rs` | the `brick` transform |
| `src/zaa.rs`, `src/zaa/tests.rs` | the `zaa` transform |
| `src/zaa/surface.rs` | where the model's surface sits inside a layer, from the outlines either side |
| `src/zaa/scout.rs` | a second reader kept a layer ahead, so the surface has a layer above to measure against |
| `src/slicer.rs` | `SLIC3R_*` settings the slicer exports to a post-process script |
| `src/bgcode.rs`, `src/bgcode/` | binary G-code container, heatshrink, meatpack |
| `src/lib.rs` | `Source` (sniff the head, refuse what is not G-code, two reads, never in memory) and `Sink` (an exclusive temp file beside the target renamed over it, the input's own line endings put back, and what a replacement cannot carry over) |
| `src/error.rs` | the `thiserror` enum, and the sentences a user actually sees |
| `src/cli.rs`, `src/main.rs` | clap derive; a G-code path, `--output`/`--verbose`/`--force`, one switch per transform and `--extra-flow`, at least one switch required |
| `benches/throughput.rs` | synthetic slice + wall clock, no framework |

## Invariants that are easy to break

- **A loop is capped wherever its column ENDS, and climbs from wherever it
  STARTS.** Whatever the slicer prints over a raised bead was metered for a full
  layer, so a bead left half a layer proud under a shoulder, shelf or counterbore
  gets about twice the material poured into it. The mirror is a column that
  begins on solid infill: its first bead has no seam under it, so raising it by
  the full offset asks it to span a layer and a half of gap metered for one.
  `Survey.uncovered` holds, per layer, the cells of that layer's walls the layer
  above does not cover; `Survey.unsupported` holds the ones the layer below does
  not. `brick::Pass::mark_columns` walks each loop's path ONCE and tests both
  against `CAP_SHARE`. Do NOT lower `CAP_SHARE`: capping a loop whose column
  carries on leaves the layer above metered against a step that is gone, trading
  a blob for a void. Do NOT split the walk back into a pass per set — it cost
  +28% of runtime where merged it costs +6%.
- **A loop's parity is NOT its column's history, so what a bead is laid ON is
  MEASURED, never inferred from the parity.** A wall renumbers whenever it gains
  or loses a loop — a flaring hull does it every few layers — and the loop then
  laid on the plane can sit directly over a bead standing half a layer proud.
  Metered for a whole layer it pours **exactly twice** what that gap holds.
  Measured on a stock 2-wall Benchy, 188 mm of internal wall path at 2.00×,
  concentrated at Z8–15 where the hull flares hardest; 2575 mm on the 1000-wall
  version. `Pass.rising`/`Pass.standing` carry one layer's raised footprint to
  the next, `Loop.on_a_raise` is that footprint tested against the loop's own
  path, and `Pass::geometry` reads BOTH ends of the span off what actually
  happened. Do NOT reuse `CAP_SHARE` for it: capping asks whether a column ENDS,
  where being wrong costs a void, and this asks what a bead SITS ON, where being
  wrong the same way costs a blob. `SEAM_SHARE` is 0.25 and sits in the empty
  valley between the two measured populations — a loop on the plane shares ≤0.1
  of its path with the raise below, a loop carrying a raised column on shares
  0.4–1.0, and `CAP_SHARE` cuts straight through the upper one. Record the cells
  during the walk `shares` already makes (`Cells::absorb`): walking a second time
  for them cost +22% of `brick` against +16% folded in.
- **A height change must never be a `G1 Z` of its own.** A Z-only move names no
  other axis, so the planner brings the toolhead to a dead stop to run it — on
  the loop's start point, which is the seam, with the nozzle primed. Measured on
  a real PETG part: 679 stops, 13.5 s of standing still, and the stringing to
  show for it. `Pass::carrier`/`ride` put the height on a move the slicer was
  already making, and `Pass::keep` holds back a tail of every line that lays no
  bead — moves, comments AND the `M73`/`M106`/`M204`/`T`/`G92` a slicer drops
  between the layer's `G1 Z` and the wall — across the `; FEATURE:` marker, so a
  region's first loop has one to ride. Draining that tail on an M-code cost 2 of
  132 raises on a stock OrcaSlicer file and all 132 once an `M73` followed every
  layer change.
- **Test fixtures must put a copy of the wall on the layers above AND below.** A
  wall that stops dead is capped and a wall that starts dead climbs, so a
  fixture whose body is the only wall in the file measures neither steady state.
  `middle_layer` repeats the body untagged on every layer for this, which means
  a body's loops are counted five times in `stats.loops`.
- **G-code is not guaranteed UTF-8.** Slicers copy object and filament names into
  comments in the host's legacy encoding. Read bytes and use
  `String::from_utf8_lossy`; never `BufRead::read_line` into a `String`, which
  fails the whole pass on one stray byte.
- **Never buffer the whole file.** `brick` may buffer the current `;TYPE:` region
  and nothing larger.
- **The surface builder is where the whole binary's time goes, and four of its
  savings are counter-intuitive enough to be undone by accident.** `--bricks` on
  an 18 MB, 925-layer duct takes 1.3 s and `--zaa` takes 15.0 s, all of it in
  `surface::Builder::build`, once a layer over about a million cells. (1) The
  chamfer kernel in `transform` is FASTER as a closure with four comparisons a
  neighbour than hand-unrolled at fixed offsets — the comparisons let the
  compiler drop the bounds check, and unrolling it cost **86%** (14.9 s to
  27.8 s), as did row slices, a `const` direction, and reducing the eight
  `min`s as a tree. (2) `mouths` clears the window's edge so a neighbour is an
  index away instead of a division, and fills a whole run of a row at a time;
  together 10.3 s to 4.8 s. (3) The three distance transforms get a thread each
  and the blur does NOT — it is a third of the work and splitting it fifteen
  ways took it from 2.5 s to 5.4 s. A spawn costs ~19 µs, which is
  `Builder::band`'s whole reason: whole rows, one band under a quarter of a
  million cells. (4) `blur` walks only the rows that hold a rise, and the row
  either side of one, because 84.5% of the field is zero.
  Nothing here can be skipped by looking at a layer first — measured on that
  duct, **0 of 925 layers** took the vertical-face early-out, **0** had nothing
  exposed, and 57%/71%/95% of cells are inside the distance the three
  transforms are read at, so bounding them buys nothing. The critical path is
  now ONE transform (HERE and BELOW 6.06 s each, ABOVE 2.43 s); going further
  needs one that splits within itself, and `std::thread::scope` is the only
  safe way to hand a thread a borrowed band, so the spawns would cost more than
  the split saves. Pinned by
  `the_last_strip_the_fade_carries_is_still_followed`,
  `splitting_a_window_between_threads_leaves_the_same_surface`,
  `a_rise_on_one_row_still_reaches_the_rows_either_side_of_it` and
  `a_pocket_a_row_cuts_in_two_is_still_one_pocket`; the real proof is
  byte-identical output on six real files.
- **Every write goes through `Sink`** — a temp file beside the target, renamed
  over it in `commit()`, so a crash leaves the original intact.
- **Match both marker dialects**: `;TYPE:` and `; FEATURE:`. A grep for one alone
  finds nothing on half the real files.
- **Test fixtures must look like real slicer output**: `G1 Z...` before the
  `;TYPE:` marker, two or more genuinely adjacent loops per wall (use `wall_of`),
  inner loop printed first, and a wall on the layer above the one under test.
  A fixture also needs five layers before it sees steady-state flow — the bed
  layer is never raised and the two above it are climbing, so anything shallower
  measures a climb (`middle_layer()` builds one correctly).
- **`gcode::write_fixed` and `gcode::number` must stay bit-identical to `core`.**
  They are a fast path, not a different answer: `write_fixed` falls back to
  `{:.N}` whenever scaling could have crossed a half-way point, and `number`
  falls back to `f64::from_str` past fifteen digits. The tests sweep millions of
  values against the standard library — never relax one to make a case pass.
- **A region marker is only ever a bare comment line.** Use `Line::marker`, not
  `Line::comment`: the stamps this tool leaves ride the `G1 Z` moves it inserts,
  so reading a trailing comment as a marker re-declares the region mid-wall.
- **Never ask whether the extruder is "drifting" — compare the value.** Whether a
  line has to be rewritten is whether the value it should now carry differs from
  the one it already has (`buffered.e == Some(value)`). A global drift flag is
  wrong inside a buffered region: `feed` reads the region to its end before
  `flush` emits any of it, so the input position sits ahead of the output and the
  two coincide by accident every so often. The line where they met came out with
  its original, stale absolute value — on a Cura file the extruder ran 0.6 mm
  backwards mid-wall. `Extruder::is_drifting` was deleted for this reason.
- **A `G92` is not an extrusion.** It sets the origin, so it never feeds
  `observe`/`advance` (`line.draws()` gates that), and `brick` flushes the
  buffered region before applying one, or the region's own moves are emitted
  after the reset and measured from the wrong zero.
- **A first or last layer belongs to an OBJECT, not to the file.** A file sliced
  to complete individual objects builds each from the bed up, so it holds several
  of each. `Survey::object_starts` finds them by comparing the lowest Z of each
  layer with the previous layer's — a Z-hop only ever raises the nozzle, so a
  per-layer minimum is the layer's own height. Measured on a real OrcaSlicer
  2-object slice: one drop, 21.8 mm to 0.2 mm at layer 109 of 218.
- **The wall flow is READ OFF THE FILE, per layer, and it is not a void being
  filled.** The only dial is `--extra-flow` (0 to 50 percent), and it names the
  extra a wall takes where the layer is as thick as the NOZZLE — a slope, not an
  absolute flow — which is what keeps an adaptive slice metered per layer.
  `Config::wall_flow` pins one absolutely and is for tests and library callers.
  Slicers space
  beads at `w - h(1 - π/4)`, not at the nominal width, and at that spacing a
  bead's area is exactly `h × spacing` — measured again 2026-08-11 on three
  real slices (0.0773 mm² metered against 0.0774 predicted; loops 0.4074 mm
  apart against 0.4071 predicted; the nominal-width model says 0.0855). So
  `void% = 21.46 × h/w` describes a slicer that does not exist. What scales is
  the *share of a bead sitting in the corner beside its neighbour*, which is
  `h / spacing`, and `brick::automatic_flow` is that slope anchored on the
  reference profile and held to `brick::flow_ceiling` — the flow at which a
  bead's edge reaches the centre of the loop beside it, `2 - h(1-π/4)/s`,
  which is the bead model's own arithmetic and not a chosen number. Do NOT
  re-derive it as a volume
  deficit; do NOT reintroduce a picked ceiling; do NOT change
  `DEFAULT_EXTRA_FLOW`, `REFERENCE_NOZZLE`,
  `REFERENCE_HEIGHT` or `REFERENCE_WIDTH` without re-checking every number in
  the README's flow tables.
- **The visible wall's inward offset must move with the flow.**
  `Pass::skin_offset()` is a method, not a field: on an adaptive slice the flow
  changes every layer, and an offset fixed at open time would disagree with it
  everywhere. Scaling without moving grows the part; moving without scaling
  shrinks it.
- **A ring does NOT close on itself, and testing that it does switches the
  offset off on every real file.** Slicers stop the last bead short of the seam:
  measured over all 308 candidate loops of two real slices, every one lands
  0.0385–0.0411 mm short and none within 1e-6. `move_walls` accepts a gap under
  one stated bead width, and sets the closing vertex to
  `offset[0] + (closes − entry)` so the gap survives — offsetting it by its own
  normal loses the whole offset and can run the bead past its own seam. Do NOT
  tighten either back: it took the Benchy from 0 to 14452 moved outer beads.
- **The layer laid on the bed is never raised, and a column climbs to its offset
  over `RAMP` (2) layers.** A bead on the plate is not pressed by the nozzle, so
  the flow a raise needs spreads sideways instead of building height — it filled
  in a Benchy's bottom nameplate, which is exactly one layer deep.
  `extrusion_factor` is one formula,
  `(layer_height + rise(k) - rise(k-1)) / layer_height`, covering the bed, the
  climb, the steady state and the cap. Do not special-case any of them back.
- **A run has to NAME a transform, and one that names neither is refused.** The
  switches are a required clap `ArgGroup`, so a bare path exits non-zero with a
  message naming both. Do NOT give either one a default and do NOT infer "both"
  from silence: this is handed the user's only copy of a file by a slicer that
  swallows everything it prints, so a transform nobody asked for is a print
  nobody can get back. Pinned by `cli::a_run_has_to_name_at_least_one_transform`
  and `end_to_end::naming_no_transform_fails_instead_of_choosing_one`. A dial
  whose transform was not named IS accepted and ignored — refusing a leftover
  word in a slicer field would fail a print over something that changes nothing.
- **Every number that reaches the nozzle is checked where it is read.** The
  binary takes a file plus `--output`/`--verbose`/`--force`, one switch per
  transform and `--extra-flow`, and nothing else — no sub-command (pinned by
  `cli::the_whole_command_line_is_a_file_two_transforms_and_their_dials` and
  `cli::the_brick_sub_command_is_gone`) — so a
  height is filtered by `scan::is_a_height` at every place it can arrive —
  slicer environment, bgcode metadata, the file's settings block and the survey
  — and a width by `scan::width` plus `automatic_flow`'s own guards.
  `is_a_height` has a CEILING as well as a floor: without one a settings line
  reading `layer_height = 1e12` parses, survives, and comes out of `zaa` as a
  commanded `Z-308749999999.600`.
- **`zaa` has no dials, and giving it one back is a regression.** How wide a
  step is worth following is a SLOPE, so `zaa::reach_for` derives it from each
  layer's own height (`height / tan(SHALLOWEST_SLOPE)`), and how finely a
  surface is sampled is half a grid cell — the grid the rise is measured on,
  whichever one this file was given — with an
  arc taking the finer of that and `chord_of(radius)`. Both were `--zaa-reach`
  and `--zaa-resolution`, and both were wrong to be: measured on real slices,
  the sampling dial changed the written heights by 0.00 µm at p99 across a 20×
  range, and the fixed 4 mm reach REFUSED a 1.9° cone (6 mm treads) that a
  wider one follows at no cost in time or memory. The reach still has to exist
  — `fading` on the strip's width is what stops a covered cell deep inside the
  part leaking a rise into the strip beside it — but a figure in mm meant 2.9°
  at 0.2 mm layers and 1.1° at 0.08 mm ones. What tells a flat top or a ledge
  from a slope is `sloped`, the layer-BELOW test, not the reach: a cube and a
  flat plate with a boss on it are byte-identical at every reach up to 50 mm.
- **The fade runs OUTWARD, past the reach, never inward inside it.** Two
  consecutive strips meet at full amplitude and at no other — with both ends
  held inside `±h/2` the only solution is `+0.5` to `-0.5` — so an amplitude
  scaled by `f` leaves a riser of `(1-f)·h` at EVERY boundary it touches.
  Tapering inside the range being followed therefore trades one step for a
  band of them: measured on a uniform slope with the taper running inward over
  the last quarter of the reach, the riser left was **1.000 h at 1.00°, 0.479 h
  at 1.15° and 0.008 h at 1.33°**, a whole staircase at slopes the tool was
  reporting as followed. `carried = reach * (1 + FADE)` is what moved it out:
  everything down to the reach is followed at full amplitude and the quarter
  PAST it is where the amplitude goes, and all three of those slopes now leave
  **0.000 h**. Pinned by
  `two_layers_of_one_slope_meet_without_a_riser_between_them`.
- **The surface grid is CHOSEN per file, resolution is the whole of the
  quality, and the BUDGET is the only ceiling on it.** `Grid::for_span` spends
  a fixed budget of cells (`surface::MAX_WINDOW`) on whatever span the part
  has, so memory is flat and only the resolution moves. `Grid::held(cell,
  coarsest)` is the one clamp, its floor is always `Grid::FINEST`, and
  `for_span` passes **no** upper bound — only `Grid::of` holds to `CELL`. Held
  to `CELL` the largest span that fits two million cells is 421.9 mm square, so
  a 450 mm square layer (2.27M cells) and a 600x300 mm one (2.02M) were refused
  outright and the transform silently did nothing on a bed-scale part; past
  that the resolution gives way instead, at 0.302 mm on 600x300 mm, 0.320 mm on
  450 mm square and 0.427 mm on 600 mm square. Do NOT put an upper clamp back
  on `for_span`, and do NOT let the wall-stacking test follow the grid down:
  `CELL` is the tolerance of "these two beads overlap" and has its own
  measurement behind it. On a 60 mm cap, weighted over the layers that leave a
  tread wider than a bead, 0.3 mm cells smoothed 0.026 of half a layer and
  0.05 mm cells smoothed 0.444.
- **`sloped` is measured against HALF a strip, not a whole one.** The tread
  below is a DIFFERENCE of two grid distances and the strip is a SUM of them,
  so the difference can be zero and the sum cannot be under a cell. Compared
  one for one, a uniform slope reads as a partial one: measured 0.368 on a
  60 mm cap where the geometry says 1.0. `SLOPE_MARGIN` is that gauge, and it
  leaves the flat-top and ledge guards byte-identical because both put the
  layer below in exactly the same place as this one and so read zero however
  generous it is.
- **What the footprint traces is a bead CENTRELINE, and the model's outline is
  half a bead outside it.** `Slice::bead` shifts the place across the strip by
  that much. It cancels in the strip's width and in the slope test, which are
  both differences of two outlines shifted alike; it does not cancel in where
  a bead sits along the ramp, and leaving it out takes the visible wall a whole
  half layer down on a tread one bead wide. At 0.3 mm cells the grid's own
  overshoot hid most of this by accident.
- **A layer with the same outline above it AND below it is not measured at
  all.** It is the middle of a vertical face: the layer above covers every cell
  of it, so nothing is exposed, and the layer below ends where it does, so
  `sloped` is zero everywhere. `Builder::build` returns before touching the
  window. Byte-identical output on six real slices, and it took the test suite
  from ~9 minutes to 55 seconds — `binary_gcode.rs` alone went from 480 s to
  4.1 s, because its 2000-layer column is nothing else.
- **A cell no flood fill can reach is NOT a cell with nothing in it, and `mouths`
  is the one place that mattered.** `mark` paints a path of bead CENTRES one cell
  wide while the plastic reaches half a bead either side, so in a solid region
  every pair of neighbouring lines leaves a speck of "hollow" between them. A
  speck passes the pocket/nesting/slack tests trivially and its carry then runs
  the whole sparse interior of the part: measured on a user's 672-layer part,
  **2878 pockets accepted, median ONE cell across, carrying a median of 1301
  rings — 25.6 million cells of the layers above carved out of 66 thousand cells
  of pocket.** `is_open` then said true all through the inside of the object, so
  `zaa` followed walls buried in there, raised them, and the next layer printed
  on top; `--zaa` re-metered that file by **+14.34%** where a followed surface
  gives back what it takes and comes out near zero. `waist` (a pocket must be a
  whole bead across in both axes) and `tread` (the carry may not outrun
  `carried`, past which `fading` is zero anyway) are the two bounds, and both are
  the module's own arithmetic. Afterwards: 272 moves on 27 layers at **-0.67%**,
  against 261 on 24 with `mouths` off entirely — so real mouths survive. Do NOT
  relax either bound to "keep more surface": a Benchy went +12.47% to +4.32% and
  an 18 MB duct went from 4213 followed moves to NOTHING, and every mouth the
  duct had was a crack two or three cells wide. Pinned by
  `surface::tests::a_gap_between_two_beads_is_not_a_hole_that_opens_upward` and
  `surface::tests::a_pocket_loose_in_a_void_is_not_a_lip_over_a_hole`, both of
  which check the accepting half too.
- **A surface is only ever reshaped where nothing is printed over it, and a wall
  only where it stands on its own plane.** `Field::is_open` and
  `zaa::Pass::reshapes` are the two guards, and both are load-bearing. The
  hidden wall IS followed, and this is where most of the smoothing comes from —
  but ONLY when bricking is not running in the same pass (`Config::bricked`).
  Bricking may have raised the bead under a hidden wall half a layer, and
  lowering onto that closes a gap the slicer metered open. Do NOT drop the
  `!self.bricked` gate; it is pinned by
  `end_to_end::the_hidden_wall_is_left_alone_when_bricking_is_running_too`,
  which diffs a `--bricks` run against a `--bricks --zaa` one.
- **Coverage is answered SAMPLE BY SAMPLE, never move by move.** A move that
  starts or ends under the layer above still follows the surface over the part
  that does not. Vetoing the whole move on its two endpoints threw away
  **1678 mm of exposed sloped path against the 624 mm it kept** on a stock
  Benchy — a zigzag over a tread begins and ends under the wall above by
  design, so nearly every pass of it was refused by its own ends. Do NOT put an
  endpoint test back; `Pass::sample` already forces a covered sample onto the
  plane.
- **A followed bead must never fall faster than a slope can, and a CLIMB is
  bounded by something else entirely.** The surface is at its highest exactly
  where the layer above begins, and a bead carrying on under it has to be back
  on its plane, so the constraint holds a half-layer step with one grid cell to
  do it in. Raw, that put **886 of 2207 written moves on a 60 mm cap steeper
  than one in two**, worst 0.1 mm over 0.023 mm of travel. `zaa::Pass::ease`
  spreads the descent out ahead of the edge at no more than one layer height
  per bead width, only ever LOWERING a sample and never touching a covered one
  — so a bead under the layer above still sits exactly on its plane. A climb is
  held to one layer height per GRID CELL instead: what bounds a descent is the
  nozzle's flat underside plowing back through material it laid a bead width
  ago, and a climb lifts away from that material into a gap the extrusion is
  already metered for. The only bound left is what the blurred field can
  express, which is a cell. Do NOT give the climb the descent's figure: the far
  edge of a strip is exactly where the ramp must reach half a layer for one
  layer's ramp to meet the next one's, so a tread narrower than a bead was
  levelled outright and the staircase came back. Pinned by
  `zaa::tests::a_surface_never_drops_faster_than_a_slope_can`.
- **The layer plane is the LOWEST height the layer commands, never the last
  one.** A Z-hop lifts, a bricked wall lifts, and `zaa`'s own output does not
  sit on one height at all. Reading a plane back out of processed G-code —
  including in an audit script — has to take it from the input.
- **A footprint walk must never step an axis that has arrived.** A move whose
  end lands exactly on a grid line crosses it at exactly the end of the move, and
  a walk that steps anyway passes its own destination and never reaches it again:
  8193 cells for a 1.5 mm move, a streak of them across the part, and the survey
  27% slower. Pinned by
  `footprint::a_move_that_ends_on_a_grid_line_stops_there`.
- **`surface::Builder::blur` is not cosmetic.** Distances quantised to the grid
  make the rise wobble around a curve, and the wobble breaks one long move
  into a dozen short ones: 54105 output moves without it against 26610 with it.
  A box blur is exact on a straight ramp, so it moves only the noise. A finer
  distance transform is NOT the answer — 3-4 to 5-7-11 moved that figure by 1%.
- **`zaa::simplify` judges a stretch by its CLIMB, so it has to be told how far
  one may run.** The samples of an arc at a steady height sit on one straight
  climb exactly as a straight move's do, and the slope range says nothing about
  where a sample is in the plane, so without the span an arc collapses onto its
  own chord: measured on a 1000-wall Benchy, **74 arcs written as a single
  straight move**, the worst a 160° sweep of a 1.5 mm radius laid 1.27 mm inside
  its own wall, which erased a 3 mm post on the stern deck. `Pass::sample`
  returns `chord_of(radius)` for an arc and `f64::INFINITY` otherwise. A test
  that only checks the written points land ON the circle does NOT catch this —
  what is printed is the chord BETWEEN two of them, so check its sagitta.
  `simplify`'s corridor is HALF the tolerance for the same family of reason:
  what gets printed is the chord from the anchor to the last sample that
  fitted, not the slope the corridor was kept for, and an interior sample can
  sit a whole corridor from that slope and another from the chord. Two halves
  are what make `TOLERANCE` describe the printed line rather than a line nobody
  prints.
- **Both passes must ask `Line::draws_in_plane`, and must ask it the same
  way.** The survey draws the cells a wall runs through and the rewrite asks
  which of them the layers either side hold, so a bead one pass counts and the
  other does not is a cell asked about that was never drawn — a column capped
  where it carries on. A bead running along one axis names ONE word, so
  demanding both `X` and `Y` reads it as a travel; an arc naming only `I`/`J`
  is a full circle, so demanding either reads it as nothing at all. Whether
  material actually came out is the caller's own question, since only the
  caller knows what the extruder has been told since.
- **A file with NO layer-change marker is laid out, not given up on.**
  `scan::Markerless` opens a layer on the first bead laid off the plane the
  last one sat on, and the survey and `brick::Pass` use that one rule — a
  boundary they disagree on is worse than no boundary at all, because every
  per-layer set is then consulted for the wrong layer. It is the Z MOVE that
  cannot be trusted: a hop lifts and comes back down before the next bead, so
  counting Z moves counted every hop as a layer and walked the rewrite's layer
  number away from the survey's. The plane is `Markerless::plane()`, measured
  at the beads rather than accumulated, because the Z that reaches a layer is
  commanded while the layer before it is still open — which is also why
  `Pass::flush_before_a_layer` writes the loops still buffered at the layer
  that is ENDING. A file that turns out to state its layers after all drops the
  lot (`Scan::forget_the_markerless_layout`); only a start G-code's purge line
  can reach that. `zaa` still needs real markers and says so.
- **Gap fill and a thin wall are FILLERS: buffered with a wall, never one of
  it.** `brick::is_filler` covers `Feature::GapFill` and `Feature::ThinWall`.
  Both arrive in the middle of a wall, so both are buffered with it — written
  straight out they would land ahead of loops the slicer laid before them, and
  the loops either side of them would fall into different contours. Neither
  takes a place in the alternation and neither is ever raised: a thin wall's
  two faces are BOTH the visible one, so half a layer of step on it is half a
  layer of step on the outside of the part. They are metered differently and
  that is deliberate — gap fill is laid into the valley between two beads and
  straddles whatever they did, so it comes out exactly as sliced, while a thin
  wall is a bead of its own with a plane under it and takes `geometry` like any
  other bead, which is also the one reason a filler may count as covering a
  raise below it.
- **A footprint walk that cannot be completed is `Trace::Refused`, and the
  caller has to see it.** Everything downstream reads a missing cell as
  "nothing was printed there" — in `brick` a column capped where it carries on,
  in `zaa` a hole in a layer's coverage — so a trace cut short in the middle is
  a lie no caller can see through, where a refusal is one every caller can. A
  refusal keeps ONLY the two cells the move's ends stand in, because that is
  where the nozzle demonstrably was and a caller sizing a window off the
  footprint has to see them; a walk cut short at `MAX_CELLS` instead left a
  trail heading off toward a coordinate no printer could reach, and a window
  was then fitted to a part that is not there. No printer makes such a move:
  warn and carry on, never fail.
- **A line read in relative or inch mode is written back exactly as found.**
  `Modal::is_plain` is `G90` and `G21` together, and it is the only state in
  which a coordinate this tool writes says what it means. A `G91`/`G20` section
  is custom G-code — a colour change, an MMU swap, a timelapse, a layer-change
  script — and never a perimeter or a top surface, so nothing is given up by
  leaving one alone. It is still MEASURED, so nothing downstream is misplaced:
  `Modal::apply` scales by 25.4 under `G20`, accumulates under `G91`, and takes
  a `G92` as a place however the mode reads a move. Feed it `Line::parse`, not
  `Line::scan` — `scan` drops `X` and `Y`, and a tracker that never sees them
  loses the position.
- **A rewritten line keeps its Marlin checksum true.** The serial dialect ends
  a line with `*nn`, the XOR of every byte in front of the `*`, and stops
  parsing there — so a word appended behind it is never seen and the height
  change silently does not happen, while a number changed in front of it leaves
  the stated sum stale and the whole line is rejected. `gcode::rewrite` puts
  new words BEFORE the `*` and recomputes the sum over what it actually wrote.
- **The temporary is created with `O_EXCL`, under a name nobody can work out
  in advance.** A slicer runs its post-processing script over a file in the
  system temp directory, which every local user may write to, so a name built
  from the target and the process id is one another user can create ahead of us
  — as a symlink onto a file of their choosing. `create_new` fails on an
  existing name of any kind, symlink included and without following it, and
  `token()` comes from `RandomState` rather than from the clock or the pid,
  which are both guessable and both repeat across machines. `destination`
  follows every symlink to the real file first, so the rename replaces the file
  rather than the link and stays on one filesystem, and the temporary is opened
  at the target's own mode before a byte is written.
- **`Sink` puts the input's own line endings back, and that is not cosmetic.**
  Every reader here strips a trailing `\r` and every transform writes a bare
  `\n`, so without `Sink::restore` a file authored on Windows comes back with
  EVERY line changed, including every line neither transform touched. A file
  that did not end on a newline does not gain one either: the last newline is
  held back until something proves it was not the final byte, and a held
  newline is deliberately not flushed. The common case costs nothing
  (`Endings::verbatim`), and the endings are read off the head already fetched
  plus at most the file's last byte — never a second pass, and never the file
  in memory.
- **A file that does not read as G-code is REFUSED, and `--force` is the only
  way past it.** The default output target is the input itself and pass-through
  runs every byte through a lossy UTF-8 repair, so a mistyped path onto a
  `.3mf`, an STL or a photo is a file nobody can get back. `Source::open`
  sniffs a fixed 4 KiB of the DECODED head — so a bgcode container is judged on
  the G-code it unpacks to — and wants one whole-line comment or one command
  word; `Source::open_forced` asks nothing. Same reasoning as refusing a run
  that names no transform. Do NOT widen the sniff into a share of the file:
  peak memory must not follow the file's size.

## Conventions

- Doc comments explain *why*, and cite the measurement when a constant encodes
  one — see `MAX_LOOP_GAP`, `PROBES` and `RAMP` in `src/brick.rs`. Do not add
  comments that restate the next line.
- No `unsafe`. Errors are `crate::Result` over a `thiserror` enum; `unwrap` only
  in tests and where a comment shows it cannot fire.
- Dependencies are deliberately few (`clap`, `crc32fast`, `flate2`, `thiserror`).
  Do not add one without saying what it replaces.
- Unit tests live in `#[cfg(test)]` modules next to the code, in a sibling
  `tests.rs` where the module would otherwise bury what it tests; behaviour that
  spans the binary goes in `tests/`.
- Commit subjects are imperative and scoped where it helps:
  `brick: number a wall's loops from the visible side`.
- Anything a slicer does that surprised you is a warning, never a hard failure —
  the user's print is already in progress.
