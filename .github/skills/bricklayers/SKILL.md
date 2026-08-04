---
name: bricklayers
description: Domain knowledge for the bricklayers G-code post-processor — how brick layering works geometrically, how perimeter loops are grouped into contours, which signals in sliced G-code are trustworthy and which are traps, real-file measurements, and an audit script for verifying output. Use when changing src/brick.rs, src/scan.rs or src/feature.rs, when a user reports that bricking looks wrong or does nothing, when reasoning about loop detection, contour boundaries, raise parity, extrusion factors, arcs, wall order, or slicer dialects, and before trusting any claim about what a slicer emits.
---

# Bricklayers

Everything here was established by measuring real sliced G-code, not by reading
slicer source or reasoning from first principles. Where a number appears, it
came from one of the fixtures listed in
[references/measurements.md](./references/measurements.md).

**The single most important rule: do not assert what a slicer emits. Measure it.**
Every wrong turn recorded here started as a confident claim that went untested.

## The geometric model

A wall is a set of nested perimeter loops. `brick` raises alternate loops by
half a layer height so that neighbouring loops bond across a staggered seam
instead of stacking their weak points.

```
        loop 1   loop 2   loop 3          <- one wall, seen end-on
layer N   ###     ...      ###
          ###     ###      ###            <- raised loops sit half a layer high
layer N-1 ...     ###      ...
```

Four consequences that are easy to get backwards:

1. **The stagger is sideways, not vertical.** The *same* loop is raised on
   every layer, so the raised loops form a continuous column. Two consecutive
   layers looking identical is correct. A layer where the pattern inverts is a
   bug.
2. **Raised loops need no extra flow *for volume* in the middle of a print.**
   The raised column has the same spacing as the unraised one, so volume
   conservation alone gives `1.0`, and `extrusion_multiplier` defaults to it.
   Anything above is a physical constant of the printer and filament — it
   compensates for a raised bead being laid against a step, and nothing in the
   G-code can fit it, so the code never picks one. The CLI accepts `1.0` to
   `1.3` and every doc example passes `1.05` as a starting point. Flow at the
   two ends of the column *is* derived, and is not this knob. Raised loops
   carry roughly a third of a part's filament (30.5% on a 240-layer Benchy), so
   `1.05` adds about 1.5% to its mass; `--verbose` prints the exact figure per
   file.
3. **First layer**: a raised loop spans from the bed to the shifted nozzle, so
   it needs `(first_layer_height + shift) / first_layer_height`. That is `1.5`
   only when the first layer equals the layer height, which is not the default
   in any slicer.
4. **Last layer**: `capping()` keeps it flat, but the loop below it left a gap
   of `layer_height - shift`, so it is metered at `0.5`. The layer this applies
   to is the last one holding an *internal perimeter*, **not** the last layer of
   the file — see the gotcha below, where testing the layer count meant capping
   never fired at all.

## Contour grouping — the hard part

One `;TYPE:` region holds far more than one wall: an island's loops, the walls
of every hole in it, other islands the slicer reached without a marker, and the
fragments a thin wall broke into. Numbering has to restart at each of them.

### What works

Two loops are the same wall when **their paths run within `MAX_LOOP_GAP`
(2.0 mm) of each other** — `Pass::adjacent` in `src/brick.rs`. A bounding-box
comparison rejects most pairs first; then up to `PROBES` (16) sampled points of
the current loop are tested against every point of the previous one, with an
early exit.

Measured over five prints: neighbouring loops of one wall run 0.4–1.5 mm apart,
the next island is more than 3 mm away, and almost nothing falls in between.

### What does not work — all three were tried and shipped before being caught

| Signal | Why it fails |
|---|---|
| **Retraction before the loop** | Slicers retract *between neighbouring loops of one wall* whenever the seams are far apart, and cross to another island *without* retracting whenever the travel is short. Mis-grouped ~2600 transitions per print. |
| **Distance from one loop's end to the next one's start** | Noise. Spreads evenly from 0.5 mm to past 10 mm, because the seam can sit anywhere around a loop. |
| **Bounding-box nesting (does one loop enclose the other?)** | Most of a wall is not a closed ring. Where the slicer follows a curve the loops are arcs offset *sideways*, each often longer than the last, so neither encloses the other. Dropped a 1000-wall Benchy from 1392 raised loops to 314. |

### Numbering direction

Which loop is number one decides which loops are raised, so it must be a loop
that stays put. **A wall gains and loses loops at its hidden end as it
thickens**, so numbering from the first-printed loop inverts the stagger every
time the count changes — every third layer or so on a Benchy hull.

`number_loops` numbers outwards from the loop against the external perimeter.
Which end that is comes from `Config::external_perimeters_first`, which defaults
to `false` because every mainstream slicer prints the visible wall last.

**Wall order cannot be read from the file.** Both attempts failed:
marker-transition counting gave 1066 vs 1042 on one file (a coin flip), and the
geometric direction is genuinely mixed within a single file because holes print
opposite to island walls. Take it from the slicer environment
(`SLIC3R_EXTERNAL_PERIMETERS_FIRST`, `SLIC3R_WALL_SEQUENCE`), from the file's
own config block (`; wall_sequence = ...`), or from `--wall-order`.

`--wall-order` takes `auto`, `external-first` or `internal-first`, and the
explicit values beat detection outright. It replaced a one-way
`--external-perimeters-first` flag that was OR'd into the detected answer, so a
file *misread* as external-first could not be corrected. That direction matters:
forcing external-first on an inner-first file took stagger inversions from 472
to 1055 of 2626 on `orig-maxwalls.gcode`.

### Lone loops

A contour holding one loop is raised like any other. It has no internal
neighbour to alternate with, but an internal perimeter exists only because the
slicer inset it from an external one, so the wall that shows always runs
beside it — and where a solid wall is about three beads thick, on both sides of
it. Measured over real slices:

| file | lone contours | share of internal-perimeter path |
|---|---|---|
| 240-layer Benchy, many walls | 510 loops, median 13.3 mm | 8.7% |
| 2-wall dragon | 3022 loops, median 6.2 mm | 60.1% |

Fewer of them are under 5 mm long than in multi-loop contours (18% vs 34% on
the Benchy), so they are walls, not slivers. Removing the old `loops > 1` guard
took the Benchy from 1413 to 1978 raised loops and a real 2-wall slice from 622
to 3622 of 4008.

The change is provably additive: comparing commanded Z move for move between
the two builds (`/tmp/zdiff.py`, geometry identical) gives exactly two deltas,
`+0.000` and `+0.100`, and **zero moves lowered** — on the Benchy 4882 of 63118
newly raised, on the 2-wall slice 89667 of 626917. Parity is unchanged on both
(22 of 276, and 99 of 379), and `invariant` stays at 0 of 284686 external
perimeters raised on the 2-wall file.

**The old reasoning here was wrong and is recorded so it is not repeated:** it
claimed raising a lone loop "lifts the whole wall clear of the visible one".
A lone *contour* means no other internal loop is adjacent, not that nothing is
adjacent. Only a single-wall region has nothing to raise.

## Gotchas

Each of these cost a wrong answer or a shipped bug.

### G-code parsing

- **`;TYPE:` is not the only region marker.** OrcaSlicer emits
  `; FEATURE: Inner wall` on some flavours. `grep '^;TYPE:'` finds *nothing* on
  those files. Always match both, and `;FEATURE:` with and without the space.
- **Arcs are extrusions.** `G2`/`G3` carry `X`, `Y` and `E`. They must open
  loops, bound the lead, and feed adjacency, or a loop opening with an arc gets
  its opening stranded in the travel and printed before the raise. Arc fitting
  is on by default in OrcaSlicer and accounted for 32% of internal wall
  extrusion in a stock Benchy.
- **Arc `E` is rescaled correctly** by `Line::write_e`, which rewrites the `E`
  word whatever the command is. Do not warn about arcs for `brick` — the loop
  around one is shifted and rescaled like any other, so the warning cries wolf.
- **Region must be reset at a layer change.** OrcaSlicer opens the next layer's
  wall with a stray segment *before* re-declaring `;TYPE:`. Carrying the old
  region across turns that segment into a one-loop perimeter region at the new
  layer's Z — 54 of them in a 63-layer file.
- **G-code is not guaranteed UTF-8.** Object names arrive in the host's legacy
  encoding. Read bytes and repair lossily; never `BufRead::read_line` into a
  `String`.
- **Leading-dot floats** (`G1 Z.6`, `E.41252`) are normal.

### Analysis scripts

- **Split regions on `;LAYER_CHANGE` as well as `;TYPE:`.** Not doing so merged
  a layer's opening segment into the previous layer's region and produced a
  completely bogus finding ("54 false splits") that drove a wrong fix.
- **A check that cannot fail proves nothing.** The first version of the
  external-perimeter invariant tracked the raise from the `raised` marker and
  cleared it on any other stamped line — including the `resume` line emitted
  immediately after every raise. Its window was one line, so it reported zero
  violations on any input, and that zero was quoted for a whole session. Derive
  state from the G-code (nozzle Z against the last commanded Z), and **write a
  positive control that the check must flag**.
- **The same trap bit `gcode.py` a second time, and it survived a year of use**
  (fixed 2026-08-03). `regions()` set `Loop.raised` from the `raised`/`reset`
  stamps. `reset` is only emitted when this tool moves Z down *itself*; at a
  layer change the slicer's own Z move lands the nozzle with no stamp at all,
  so a loop raised at the end of a layer left the flag set and mislabelled
  every loop after it. On a real 2-wall slice it reported 79 inversions before
  a change and 115 after, when a move-for-move Z comparison proved **not one
  commanded Z had changed**. It now derives the state from Z. Any parity or
  contour figure recorded before that date is suspect — the Benchy baseline it
  reported as 8 is really 22.
- **Single-layer synthetic fixtures are invalid.** Layer 0 is also the last
  layer, so `capping()` suppresses the raise and every result is misleading.
  Two bad conclusions came from one such fixture.
- **A fixture whose wall sits on the file's last layer is invalid too**, and it
  hid a real bug for months. `capping()` used to test the layer count, and on
  six real Orca slices the last layer holding an internal perimeter is *never*
  the last layer — solid infill, a top surface or ironing is printed over the
  walls, and Orca closes a file with a layer marker whose only extrusion carries
  no `; FEATURE:` line at all. The gap runs one to five layers, so capping fired
  on a layer with no wall on it every single time and every part's topmost wall
  loop stood 0.1 mm proud. It is `Survey::object_tops` now. Give any fixture a
  wall on the layer above the one under test.
- **Test fixtures must be real geometry.** Side-by-side segments are separate
  contours and never brick. Concentric squares printed *innermost first* are
  what slicers emit; the highest-numbered loop is the one against the visible
  wall and is the one raised.

### Tooling

- `run_in_terminal` sometimes rewrites a command; pass absolute paths and avoid
  relying on `$TMPDIR` expansion inside quoted subshells.
- Heredocs get mangled — an exclamation mark can arrive escaped, breaking
  Python `!=`. Write analysis scripts to a file instead of piping a heredoc.
- `fetch_webpage` silently summarises JSON. Use `curl` + `jq` for anything that
  must be complete.

## Verifying a change

Never ship a change to loop handling without running all of these.

```sh
cargo fmt --all --check
cargo clippy --all-targets -- -D warnings
cargo test
cargo run -- brick --extrusion-multiplier 1.05 --verbose --output /tmp/processed.gcode <real.gcode>
python3 .github/skills/bricklayers/scripts/audit.py all /tmp/processed.gcode
```

The audit script checks, on real output:

| Check | Must be |
|---|---|
| `invariant` — external perimeter extruded while the nozzle is raised | **0**, and it exits non-zero when it is not |
| `parity` — layer-to-layer inversions of the outermost loop | as low as possible; 22 of 276 regions is the current Benchy baseline |
| `contours` — loops per contour, and why each contour ended | mostly multi-loop on a many-walled model |
| `adjacency` — distance between consecutive loops | two clusters, nothing in between |
| `arcs` — share of extrusion emitted as `G2`/`G3` | informational |

Confirm `invariant` still has teeth after touching it:

```sh
printf '%s\n' "M83" ";LAYER_CHANGE" "G1 Z0.200 F600" ";TYPE:Perimeter" \
  "G1 Z0.300 F600 ; bricklayers brick raised" ";TYPE:External perimeter" \
  "G1 X20 Y0 E0.5" > /tmp/control.gcode
python3 .github/skills/bricklayers/scripts/audit.py invariant /tmp/control.gcode
# must report 1 violation and exit 1
```

Compare raised-loop counts before and after against
[references/measurements.md](./references/measurements.md). A large drop means
contours are being split that should not be.

## Where things live

| File | Holds |
|---|---|
| `src/brick.rs` | loop buffering, `assign_contours`, `number_loops`, `adjacent`, `extrusion_factor` |
| `src/scan.rs` | `Survey` — one pre-pass for layer count, heights, feedrate, arc count, transform stamps |
| `src/feature.rs` | region marker classification for every slicer dialect |
| `src/slicer.rs` | `SLIC3R_*` settings the slicer exports before running the script |
| `src/gcode.rs` | byte-scanner line parser, `Code`, `Extruder` |

## Upstream

This is an independent Rust reimplementation of
[TengerTechnologies/Bricklayers](https://github.com/TengerTechnologies/Bricklayers).
The triage of that project's issues and pull requests — what is a real defect,
what was refuted by measurement, and what is still unverified — is in
[references/upstream-tracker.md](./references/upstream-tracker.md).
