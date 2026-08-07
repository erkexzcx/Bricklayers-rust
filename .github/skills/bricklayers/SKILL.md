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
3. **The bed layer is never raised, and a column climbs over the two layers
   above it.** Displacing a column upwards opens a half-layer void under its
   bottom bead that has to be extruded once, whichever layer starts it. Asking
   one bead for all of it leaves the nozzle half a layer clear of the surface
   it lays against, so it presses nothing and the surplus spreads sideways —
   on a Benchy that filled in the bottom nameplate, which is exactly one layer
   deep. `RAMP` (2) spreads it: no bead spans more than a quarter of a layer
   beyond what the slicer metered it for, and the layer on the plate comes
   through byte for byte.
4. **One formula covers all of it.** `extrusion_factor` is
   `(layer_height + rise(k) - rise(k-1)) / layer_height`, where `rise` is the
   column's offset `k` layers into the object. It falls out at 1.0 on the bed
   (nothing has risen yet), 1.25 on each climbing layer, 1.0 once the column is
   up — which is where `extrusion_multiplier` applies instead — and 0.5 where
   `capping()` forces the offset back to zero. A column capped before it
   finished climbing gives back only what it took, e.g. 0.75.
5. **A column has to be capped wherever it ends, not only at the top of the
   part.** Whatever the slicer prints over a raised bead was metered for a full
   layer, and a bead standing half a layer proud leaves it half a gap. That is
   only harmless where the thing above is the same column, raised too. A
   shoulder, a shelf, a counterbore or a screw-head recess ends one column
   partway up while the rest of the part carries on, and the surface that
   closes it then lays about twice the material it has room for. Measured on
   the bushing this was found on: 293.8 mm of a 399.0 mm top surface sat on a
   bead 0.1 mm proud, 8.84 of that layer's 12.01 mm of filament going into half
   a gap, with the fan off. Testing the object's last wall layer alone — which
   is what `object_tops` does — caught the part's own top and nothing else.
6. **`layer_height` in that formula is the layer's OWN height, and so is the
   one behind `rise(k-1)`.** Where a slicer varied the height, half of one
   nominal is wrong nearly everywhere: measured on an adaptive Benchy the
   layers run 0.0808 to 0.1186 mm while the profile still says `0.2`, and
   raising by half of 0.2 lifted **383 of 511 layers past their own height**,
   leaving the bead in air with a gap beneath it. `Survey::layer_heights`
   measures each layer as `Z(k) - Z(k-1)`, which is in the past, so the
   streaming design needs no lookahead. Note the consequence for flow: when a
   layer thins, its column below already fills part of it, so the bead is
   metered for the gap that is left — a 0.1 layer over a 0.2 one takes **0.5×**.

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

### Is anything standing on this loop?

- **The answer comes from the survey, not the rewrite.** It needs the *next*
  layer's geometry, which a streaming rewrite cannot see. `Scan` traces each
  layer's internal perimeters into `footprint::Cells`, keeps two layers at a
  time, and stores only the difference — the cells of a layer that the one
  above does not hold. That set is small by construction (it is the part's
  ceiling area), so peak RSS stayed at 14.0 MiB on a 58 MB two-dragon slice.
- **Cells, not points.** `CELL` is 0.3 mm, about two thirds of a bead: two
  paths that share a cell overlap by more than half their width. The tolerance
  is not arbitrary — measured over three real slices, 96.3 to 96.7% of wall
  path has a wall within 0.2 mm of it on the layer above and the rest is spread
  from 0.4 mm out to 3 mm. Anything in that window separates the two cases.
- **Cap only a loop that has genuinely run out** (`CAP_SHARE`, 0.75). Capping
  one whose column carries on above would leave the layer above metered against
  a step that is no longer there, i.e. it would trade a blob for a void. The
  per-loop distribution is close to binary anyway: 91 to 97% of loops are
  covered almost end to end, and most of the rest are uncovered end to end.
- **Arcs must be followed round, not cut across.** 57% of the bushing's wall
  moves are `G2`/`G3`, and a whole ring arrives as two or three of them. Taking
  their chords would report every ring as covering nothing and cap the lot.
  `Line::arc()` carries `I`/`J` and the direction; `G2` is clockwise.
- **The survey and the rewrite must agree on what extrudes.** The rewrite asks
  about cells the survey drew, so `Scan` uses the same test the rewrite opens a
  loop with (`delta > 0` and both `X` and `Y` present). A stricter test in the
  survey would make loops look uncovered that are not.
- **Verify with the physical quantity, not the internal one.** The check that
  matters is "how much solid-infill or top-surface path is laid over a bead
  standing half a layer proud", measured straight off the output file. On the
  bushing it went 293.8 mm to 0.0 mm. A count of capped loops proves nothing.
- **Cost, measured on nine real slices:** 0% to 11.5% fewer raised loops, and
  across all of them zero moves were raised that were not raised before and no
  commanded geometry changed. Survey throughput fell from 596 to 204 MB/s on
  the synthetic wall-heavy bench; on real files the whole run went 152 to
  250 ms (29 MB) and 297 to 494 ms (58 MB).

### How a height change reaches the machine

- **A `G1 Z` of its own stops the toolhead dead.** It names no other axis, so the
  planner cannot blend it with the moves either side, and the nozzle sits still
  and primed over the loop's start point while the axis crawls. Every loop
  starts at the seam, so an aligned seam stacks all of that ooze into one
  column. Measured on a 77-layer PETG part: **679 such stops, 67.5 mm of Z
  travel, 13.5 s of standing still on a 12m14s print**, and 145 of them landing
  on the *visible* wall's own start point. Confirmed on a print: removing them
  took 90–95% of the stringing away.
- **`Pass::carrier`/`Pass::ride` put the height on a move the slicer was already
  making.** The carrier is the last *positioning* move of the range, and it must
  carry no `E` (an extrusion or a wipe follows the layer below and cannot be
  tilted) and no comment (that is where the stamp goes, and `Survey::bricked`
  needs it to refuse a second pass). Never ride a move whose own Z is above the
  target: that is a Z-hop, and flattening one drags the nozzle through what it
  was lifted to clear.
- **The travel that opens a wall is emitted before the `; FEATURE:` marker**, so
  `Pass::keep` holds a tail of non-extruding moves and comments back across the
  marker and lets the first loop's lead reach into it. Without that tail every
  region's first raised loop needs a stop of its own: 23 of them on the same
  file, 1805 on a 3h print. The tail reuses `buffer`/`replay`, so the extruder's
  observe-then-advance ordering is the one already tested; it drains on anything
  that is not a lead line, and `TAIL` caps it so nothing larger than a region is
  ever held.
- **Do not measure the Z feedrate over the whole file.** `Scan` takes the
  slowest Z-only move, which before the `open_layer` gate was the start
  G-code's bed-clearance move on **every one of 28 real slices** — `G1 Z5 F300`
  against an in-print F600.
- **`--reorder-loops` now buys nothing.** It existed to pay for a stop once a
  layer instead of once a loop; with the height riding a travel it removes zero
  stops and still adds 19.1 m of travel.
- Result over eighteen real slices: stops down 98% to 100% (26465 → 345,
  13169 → 167, 679 → 0), line conservation exact on every one, audit invariant
  0 everywhere, no bead moved, 0.52 → 0.57 s and 13.9 MiB on a 58 MB file.

### A column that starts partway up

- **The mirror of capping, and it was a documented limit until it was measured.**
  A column beginning on solid infill — under a shelf, over a bridged hole — has
  no seam beneath its first bead, so raising that bead by the full offset asks
  it to span a layer and a half of gap the slicer metered for one. Measured
  before the fix: **2.4% to 2.9% of internal perimeter path** across three real
  slices.
- **No per-column state across layers is needed, and the old note saying
  otherwise was wrong.** `Scan::close_footprint` already holds two layers;
  `here.without(&below)` is the mirror of the `below.without(&here)` it already
  computes, and `Survey::unsupported` keeps it per layer. A loop is then dated
  by testing two sets: mostly in `unsupported[layer]` means the column starts
  here (`steps` 0), mostly in `unsupported[layer - 1]` means it started one
  layer ago (`steps` 1), otherwise the object's own count. `RAMP` is 2, so two
  sets are as far as the arithmetic ever looks.
- **Walk the loop's path once, not three times.** `Pass::mark_columns` tests all
  three sets in a single walk; doing it in separate passes cost +28% of runtime
  where merged it costs +6%.
- **Verify with commanded Z, move for move, against a build without it.**
  Measured over five real slices: **zero beads lifted**, XY unchanged, 0.28% to
  0.88% of beads lowered, and every flow change is one of four principled
  values — 0.80 (metered for a climb it was not doing), 0.952 (raised at full
  offset with nothing beneath), 1.19 (now correctly climbing), 2.0 (a
  one-layer island that had been giving back half a layer it never took).
- **Support means an internal perimeter below, and that is the right test.**
  The question the flow asks is how far below the nozzle the surface is, and
  that is the plane unless *this same raised column* stands there. Solid infill
  below and nothing below give the same answer.

### Stringing is not a retraction bug — this was checked

A user reported stringing and blamed `--extrusion-multiplier` for desyncing
retraction. Measured on their file and refuted:

| Claim | Measurement |
|---|---|
| retraction is not scaled with the flow | 184 retract/prime cycles, the only imbalance is the start G-code purge and ±0.00002 of slicer rounding |
| the multiplier over-extrudes somewhere | move-for-move `E` ratio against the input is `{1.0, 1.05, 1.25 ramp, 0.5 cap}`, max 1.2568, zero XY changes |
| the tool changes travels | it emits none, and rewrites none |

`replay` applies `delta * factor` to negative `E` too, so a retract and wipe
inside a raised loop's range are scaled — but the prime that answers them is in
the same range and gets the same factor, so it cancels (0.84 against 0.84). Do
not "fix" it.

### Variable / adaptive layer height

- **Measured, never declared.** `; layer_height = 0.2` and
  `SLIC3R_LAYER_HEIGHT` state the *setting*, not what each layer came out at.
  On the adaptive Benchy the declared value matched **no layer in the file**.
  Only `--layer-height` overrides a measurement; the exported nominal must not,
  or the fix is dead in the case it exists for — a slicer post-processing hook.
  There is **no `adaptive_layer_height` key** to grep for. The Z steps are the
  only signal.
- **The gate is load-bearing.** `Survey::layer_heights` is populated *only*
  when the heights genuinely vary, so a fixed-height file takes the exact code
  path it took before. Fixed-height files measure a spread of 3.6e-15 mm (float
  noise from subtracting printed Z); the adaptive one spreads 0.038. Thirteen
  orders apart, hence `SAME_HEIGHT = 0.001`. Without the gate, `shift` shifts in
  its last bits and E words move by one in the last digit across the whole file.
- **Exclude layer 0 and every object start from the variation test.** First
  layer height is its own setting and is routinely thicker. Counting it makes
  *every* stock profile look adaptive. Their heights are never read anyway —
  `rise(0)` is 0.
- **`(h + x) - x` is not `h` in binary** (0.2+0.1-0.1 = 0.20000000000000004).
  The `offset == rise_below` short-circuit in `span()` is what keeps a
  steady-state factor exactly 1.0. Do not "simplify" it away.
- **Measuring from printed Z is correct, not a rounding bug.** Z is emitted to
  three decimals, so a measured height can sit up to 0.001 off the slicer's own
  `; LAYER_HEIGHT:` comment. The commanded Z is what the machine executes, so
  the commanded difference *is* the layer. Do not switch to the comment — it is
  a Bambu/Orca dialect and it describes intent, not motion.
- **Orca commands Z on `G2`/`G3` arcs** (557 of them on the adaptive Benchy —
  helical lifts). `Scan` ignores arcs for Z, which is right: they are hops. Do
  not "fix" this without re-measuring.
- **A file with no layer-change marker at all still gets one flat raise.**
  `Scan` only opens a layer on a marker, so `layer_heights` stays empty and the
  nominal is used. Verified: the same synthetic slice raises 0.050–0.100 with
  `;LAYER:n` present and a flat 0.100 with the markers stripped. Left alone on
  purpose — without a marker a priming lift reads as a layer, so measuring
  heights there would feed the arithmetic garbage, and falling back is the safe
  answer. Every mainstream slicer emits one of the three markers.

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
- **A stamped travel is still a travel, and a stamped line is not the plane.**
  Once height changes ride existing moves, `gcode.py` broke twice over in one
  go: `regions()` skipped every stamped line before setting `travelled`, so the
  loops either side of a ridden travel merged, and it read the layer's height
  off the last un-stamped `G1 Z`, which is exactly the hop restore a raise now
  gets written onto. Together they reported **1784 inversions of 5252 where the
  truth was 951**, and a move-for-move Z comparison proved only 4444 beads of
  1.4 million had changed at all. `planes()` now takes each layer's height as
  the lowest Z commanded in it — a hop and a raise both only ever lift, so a
  layer's floor is the layer. The `invariant` check was unaffected and its
  control still fires on both forms; check both.
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
- **A fixture needs five layers, not two.** The bed layer is never raised and
  the two above it are climbing, so a wall has to sit on layer 3 or higher
  before it sees the steady-state flow. `middle_layer()` builds that; anything
  hand-rolled shallower measures a climb and reads as a bug in the multiplier.
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
