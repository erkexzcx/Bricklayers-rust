---
name: zaa
description: Domain knowledge for the Z anti-aliasing transform — how a layer's surface is recovered from the outlines either side of it without the model, which regions are reshaped and which are left where the slicer put them, how a stretch is metered for the gap it really crosses, the traps in doing this from G-code alone, real-file measurements, and an audit script. Use when changing src/zaa.rs, src/zaa/surface.rs or src/zaa/scout.rs, when a user reports that a shallow top still looks stepped or that a surface came out lumpy, when reasoning about the exposed strip, the reach, the grid, coverage, the sampling and the simplifier, and before trusting any claim about what a slicer leaves exposed.
---

# Z anti-aliasing

A layer is flat and the model it came from usually is not, so wherever a part's surface is shallower than about 45° the print leaves a staircase: each tread is one layer's worth of surface laid at one height, each riser is the full layer height, and the treads are what catch the light. `zaa` follows the surface across each tread instead, varying the height of the extrusion within the layer by up to half a layer either way and metering each stretch for the gap it actually crosses.

Everything here was established by measuring sliced G-code. **Do not assert what a slicer leaves exposed — measure it.** The single most expensive wrong turn in this module started as a confident claim that a top-surface region covers the exposed strip. It does not; see [The strip is mostly wall](#the-strip-is-mostly-wall).

**Load [.github/skills/bricklayers/SKILL.md](../bricklayers/SKILL.md) as well before changing anything that both transforms touch.** They compose in one pass and they share `feature.rs`, `footprint.rs`, `gcode.rs` and `scan.rs`.

## The geometric model, and why it needs no mesh

Every other implementation of this idea raycasts the original mesh. [Theaninova/GCodeZAA](https://github.com/Theaninova/GCodeZAA) asks you to export an STL per object and casts a ray up and down from every sample point; [adob/BambuStudio-ZAA](https://github.com/adob/BambuStudio-ZAA) does it inside the slicer, where the mesh is right there. A post-processor is handed G-code and nothing else, so this recovers the surface from the file.

It can, exactly, and the reason is that **a slicer takes its cross-section through the middle of a layer**. PrusaSlicer and everything descended from it slice at `print_z - height/2`. So:

- a layer's outline is where the model's surface passes `plane - height/2`;
- the next layer's outline is where it passes `plane + height/2`;
- between the two — the strip of a layer that nothing is printed over — the surface climbs from one to the other.

Linear interpolation across that strip reproduces a flat slope **exactly**, and a flat slope is the case that stair-steps. What it cannot reproduce is a surface that curves sharply inside one strip, and there it errs toward the plane.

```
                       plane + h/2   ___----   <- where the layer above begins
   layer k's strip              ___---
                       plane ---
                          ___---
        plane - h/2   ---                      <- where this layer's own outline is
                  |<------- strip ------->|
```

Consecutive strips meet exactly: layer k ends half a layer above its plane where layer k+1 begins half a layer below its own. That is what turns a staircase into one continuous ramp rather than into a shallower staircase.

README.md's `img/contour-*.png` draws exactly this, generated from the same arithmetic — see [.github/skills/diagrams/SKILL.md](../diagrams/SKILL.md). `scripts/surface.py` there is the Python twin of `src/zaa/surface.rs`, and `scripts/pin.py` puts a synthetic wedge through the compiled binary and fails if the two disagree by more than the footprint grid can account for. Measured on that wedge: the median height the binary writes is 0.005 mm off the model against 0.0075 mm of grid.

`src/zaa/surface.rs` computes it, per cell of a [`footprint`](../../../src/geometry/footprint.rs) grid chosen for this part (see [The grid is chosen, not fixed](#the-grid-is-chosen-not-fixed)):

```
dOut   = distance to the outside of layer k
dIn    = distance to the inside of layer k+1
dDown  = distance to the outside of layer k-1
bead   = half the width of the wall that traced the outlines
strip  = dIn + dOut
carried = reach * (1 + FADE)
rise   = ((dOut + bead) / strip - 0.5) * sloped * fading
         sloped = clamp((dDown - dOut) / (strip * 0.5), 0, 1)
         fading = clamp((carried - strip) / (reach * FADE), 0, 1)
```

Three of those terms are not obvious and all three were measured into place:

- **`bead` shifts the place across the strip, not the strip.** What the footprint traces is a path of bead **centres**, and the model's own outline runs half a bead outside it — on both layers alike, so it cancels in `strip` and in `sloped`, which are differences of two outlines shifted alike. It does not cancel in `dOut / strip`. Leave it out and a bead of the visible wall reads as sitting on the outline itself, and on a tread one bead wide it is taken a whole half layer down onto the layer below.
- **`sloped` is gauged against HALF a strip.** The tread below is a *difference* of two grid distances, which can be zero; the strip is a *sum* of them, which cannot be under one cell. Compared one for one, a uniform slope reads as a partial one — measured 0.368 on a 60 mm cap where the geometry says 1.0, so the transform was delivering a third of what it had measured. Half a strip is the grid's own resolution stated as a ratio, and it leaves the two guards below byte-identical because both put the layer below in exactly the same place as this one, which reads zero however generous the gauge is.
- **`fading` runs OUTWARD, past the reach, and never inward inside it.** Two consecutive strips meet at full amplitude and at no other: layer k ends `a·h` above its plane and layer k+1 begins `b·h` above its own one layer higher, so they meet only where `a - b = 1`, and with both held inside `±h/2` the one solution is `+0.5` to `-0.5`. An amplitude scaled by `f` therefore leaves a riser of `(1 - f)·h` at **every** boundary it touches, so tapering inside the range being followed does not soften the last step — it trades one step for a band of them. Measured on a uniform slope with the taper running inward over the last quarter of the reach, the riser left was **1.000 h at 1.00°, 0.479 h at 1.15° and 0.008 h at 1.33°**: a whole staircase, at slopes the tool was reporting as followed. `carried = reach * (1 + FADE)` moved it out. Everything down to the reach is now followed at full amplitude and meets exactly, and the quarter *past* the reach — a surface shallower than the tool claims to follow, and flat as sliced anyway — is where the amplitude goes. All three of those slopes now leave **0.000 h**. Pinned by `two_layers_of_one_slope_meet_without_a_riser_between_them`.

Two things have to be told apart from a slope, and both fall out of that arithmetic rather than being special-cased:

- **A flat top.** Nothing is printed above it anywhere, so the strip has no far edge. `dIn` is unbounded, the strip is unbounded, `fading` is zero and the surface is left alone. Lowering a flat top by half a layer and metering it for half a gap would starve a surface that was correct as sliced.
- **A ledge with a wall standing on it.** The strip looks like a slope's, and it is not one: the model's surface stops dead at the ledge's edge instead of carrying on down. The layer **below** tells them apart. Under a uniform slope it reaches one strip further out; under a vertical face it stops in the same place, `sloped` is zero, and the ledge stays flat.

## What is reshaped, and what is not

**Top surface, ironing, and the walls. Hidden walls only when bricking is not running in the same pass.**

### The strip is mostly wall

The obvious scope is the top-surface region, and on its own it does almost nothing. Measured on a 60 mm spherical cap sliced with OrcaSlicer 2.4.2 at 0.2 mm layers: **12 `Top surface` regions in 45 layers**, and following only those reshaped 114 moves on 11 layers. Adding the visible wall took it to **4456 moves on 29 layers**.

The reason is arithmetic. A tread is `height / tan(slope)` wide, and the wall stack standing on it is around 0.87 mm for two walls at 0.45 and 0.42. At 0.2 mm layers those are equal at about **13°**: any slope steeper than that leaves a tread the walls cover completely, so the slicer emits no top-surface region at all and the staircase is made entirely of wall.

It goes further than that. Weighted over the layers of the 60 mm cap that leave a tread wider than a bead — the only ones with a staircase to remove — the path breaks down as **12.2% visible wall, 11.7% hidden wall, 7.9% top surface**, the rest being infill under a layer. So the hidden walls are 37% of everything exposed, and following them takes the smoothing from **0.084 of half a layer over 24% of that path to 0.301 over 49.8%**.

### Why the visible wall is always safe, and a hidden one only sometimes

Two guarantees, and only the visible wall has both unconditionally.

1. **Bricking never raises it.** So a bead of it standing on its own plane is a bead nothing else has moved, and `Pass::reshapes` tests exactly that: `commanded == Some(plane)`.
2. **What sits under it is flat.** On a slope, what is under layer k's visible wall is the outermost hidden loop of layer k-1 — and that is the loop bricking **caps**, because layer k's own hidden walls have moved a whole strip inward and no longer cover it. So the gap under it is one whole layer, which is what the slicer metered it for. Where the tread is narrower than a bead, the visible wall sits over the layer below's visible wall instead, which is also never raised.

A hidden wall has the first guarantee only when bricking is not running. The bead under one of them is a hidden loop of the layer below, offset by the tread rather than the one directly beneath, and bricking may have raised that loop half a layer — lowering onto it closes a gap the slicer metered open, which is the blob that bricking's capping exists to prevent. Note that "the bead below is flat" **cannot** be read from the stream: the transform is one layer downstream of where that bead was written. So the rule is the switch, not the geometry: `Config::bricked` is set when bricking runs in this pass **or when the survey found this file was bricked by an earlier one** (`bricks || survey.bricked`) — the bead under a hidden wall may stand half a layer proud either way, and nothing in the stream says which — and `reshapes` returns false for `InternalPerimeter` whenever it is. `end_to_end::the_hidden_wall_is_left_alone_when_bricking_is_running_too` diffs a `--bricks` run against a `--bricks --zaa` one and requires the hidden wall's heights to match exactly.

### Coverage decides, not the label

Cura calls both faces of a part `SKIN`, so the underside of a sloping part arrives labelled exactly like the top of one. Nothing rests on the label: `Field::is_open` is `inside(layer k) && !inside(layer k+1)`, and a bead with anything printed over it is laid against, so it stays on its plane.

**That is decided sample by sample, not move by move.** A move used to be dropped whole where *either* endpoint was covered, and on a real part that throws away most of what there is to smooth: measured on a stock Benchy, 1678 mm of exposed sloped path was refused against the 624 mm it kept, and on the 60 mm cap the top surface lost 126 mm of 184. A zigzag over a tread starts and ends under the wall of the layer above by design, so almost every pass of it was vetoed by its own ends. Now the whole move is sampled and each sample answers for itself — which is what `sample` already did for the middle of a move. Pinned by `zaa::tests::a_bead_that_runs_under_the_layer_above_still_follows_the_part_that_does_not`.

## Doing it from G-code alone: the traps

- **A hole reads as material, unless the layer above opens it wider.** `inside` is "not reachable from the border of the window", which a through-hole's interior is not. That is deliberate — sparse infill leaves a layer's interior mostly empty, and the strip has to be measured against what the outline *encloses* rather than against where plastic happens to sit. Left there it swallowed every upward-facing surface around a hole: a countersink, a chamfered bore mouth or a funnel showed its staircase exactly as plainly as an outer slope. `surface::mouths` is what separates the two, and it needs five tests at once — the pocket has to be hollow in the layer above **all the way across**, every cell **bordering** it has to be hollow there too (nesting, not mere containment, which is what keeps a part's own interior and a shell whose wall moves inward out of this), the layer above has to carry it at least `MOUTH_SLACK` (2) cells further, the pocket has to be at least **one bead across**, and the carry may not run further than **the widest strip the transform follows**. The first is what makes it safe over sparse infill, whose gaps the next layer's lines cross; the third is what refuses a 2D honeycomb, whose two layers are identical. The last two are [what a pocket is not](#a-pocket-is-not-a-gap-between-two-beads). A matched pair is carved out of both layers, so the tread's low end is this layer's outline and its high end the layer above's — the same climb as any other slope, only facing inward. A straight bore has no tread at all and is still left exactly as sliced, and a tread narrower than a cell is invisible here whichever way it faces.
- **The plane is the lowest height the layer commands, not the last one.** A Z-hop lifts, a bricked wall lifts, and this transform's own output does not sit on one height at all. `Pass::plane` takes the minimum since the layer marker. Anything that reads a plane back out of the output — including [scripts/audit.py](./scripts/audit.py) — has to take it from the **input**.
- **A layer needs a marker.** Without `;LAYER_CHANGE`, `; CHANGE_LAYER` or `;LAYER:n` there are no layers to compare and the transform does nothing at all, including levelling: `Pass::lifted` is what keeps a file it never touched byte-identical.
- **Objects printed one at a time.** The layer under an object's first is another object's. `Pass::build` drops `below` at an object start and `above` before the next one, from `Survey::object_starts`.
- **The grid wobbles.** Distances quantised to a cell make the strip's measured width differ by up to a cell from one point of a curve to the next, and the rise wobbles with it. `Builder::blur` is a 3×3 box blur over the rise field, and it is load-bearing: measured on the 180 mm cap with only the blur switched off, the same surface came out as **54105** moves against **26610** with it. A box blur is exact on a straight ramp, so only the noise moves. Going from a 3-4 chamfer to 5-7-11 moved that figure by 1% — what wobbles is the grid, not the direction.
- **A vertical face is answered without measuring it.** A layer whose outline is exactly the one above it *and* the one below it is the middle of a wall: the layer above covers every cell of it so nothing is exposed, and the layer below ends where it does so `sloped` is zero everywhere. `Builder::build` returns before touching the window. Byte-identical output on six real slices, and `tests/binary_gcode.rs` — whose fixture is a 2000-layer square column — went from **480 s to 4.1 s**.

### A pocket is not a gap between two beads

The most expensive assumption in `mouths` was that a cell the flood fill cannot reach is a cell with nothing in it. It is not. What `mark` paints is a path of bead **centres**, one cell wide, and the plastic reaches half a bead either side of it — so on a 0.05 mm grid a 0.45 mm bead is nine cells of material painted as one, and the eight left clear beside it read as void. In a solid region, where the slicer lays lines about a bead apart, that leaves a speck of "hollow" between every pair of neighbouring lines.

Each speck is then a pocket, and it passes the first three tests trivially: one cell is hollow in the layer above wherever that layer's own lines happen not to fall, the four cells around it are this layer's beads and the layer above did not repeat them exactly, and the carry runs *forever*, because what a speck in the middle of a part sits in is the part's entire sparse interior. Measured on a user's 672-layer part sliced at 15% gyroid with 0.42 mm beads and two walls:

| | |
|---|---|
| pockets accepted as mouths | 2878 |
| their size | **median 1 cell**, 90th percentile 4, widest run median 1 |
| rings each carry ran | **median 1301** — about 190 mm of claimed "tread" |
| cells of the layers above carved | **25.6 million**, out of 66 thousand cells of pocket |

With the interior of every layer carved out of the layer above, `is_open` answered *true* all through the inside of the part, and `zaa` duly followed the walls buried in there and raised them — beads the next layer then printed straight on top of. It shows in one number: `--zaa` re-metered that file by **+14.34%**, where a followed surface gives back at one end of a strip what it takes at the other and comes out near zero.

Two bounds fix it, and both are the module's own arithmetic rather than a tolerance:

- **A pocket must be at least one bead across**, in both axes, or it is the gap between two beads. `waist` is `bead width / cell`, floored at `MOUTH_SLACK` for a file that states no width — that is already the narrowest thing this grid can express.
- **The carry may not run further than `carried`**, the widest strip followed. Past that a strip's `fading` is zero, so no cell out there could move a bead in any case; a carry that runs further has not found a lip, it has found a void. The walk stops and the pocket is refused with it.

Afterwards, on that same file: **272 moves on 27 layers, re-metered by −0.67%**, against 261 on 24 layers with `mouths` switched off entirely — so genuine mouths survive the bounds. `audit.py flow` recovers what was sliced to 0.03%, `audit.py surface` reports the outer wall in −0.043..−0.001 and 221658 moves outside a surface untouched, and bricking's invariant is 0 of 149216 on the composed output. Two real slices sitting in this repo's usual sweep were carrying the same defect quietly: a stock 2-wall Benchy went from +12.47% to +4.32% and an 18 MB duct from 4213 followed moves at +9.67% to **nothing at all** — every mouth it had was a crack two or three cells wide.

Pinned by `surface::tests::a_gap_between_two_beads_is_not_a_hole_that_opens_upward` and `surface::tests::a_pocket_loose_in_a_void_is_not_a_lip_over_a_hole`, both of which check the accepting half as well, so neither can pass by refusing everything.

## The grid is chosen, not fixed

`Grid::for_span` spends a fixed budget of cells (`surface::MAX_WINDOW`) on whatever span the part has. Memory is therefore flat and only the resolution moves: a 20 mm part and a bed-filling one cost the same, and anything up to about 70 mm across reaches `Grid::FINEST` (0.05 mm).

**The budget is the only ceiling, and `CELL` is not one.** `Grid::held(cell, coarsest)` is the single clamp; its floor is always `Grid::FINEST`, and `for_span` passes **no** upper bound — only `Grid::of` holds to `CELL`. Held to `CELL` the largest span that fits two million cells is 421.9 mm square, so a 450 mm square layer wants 2.27M cells and a 600×300 mm one 2.02M: both were refused outright, and on a bed-scale part the transform silently did nothing. Past that the resolution gives way instead — at two million cells, **0.302 mm on 600×300 mm, 0.320 mm on 450 mm square and 0.427 mm on 600 mm square**, which is the largest bed there is.

This is not a micro-optimisation — **resolution is most of the quality**. Weighted over the layers of a 60 mm cap that leave a tread wider than a bead:

| cell | mean \|rise\|, share of half a layer | share of that path touched |
|---|---|---|
| 0.30 mm | 0.026 | 13.6% |
| 0.15 mm | 0.117 | 48.0% |
| 0.10 mm | 0.237 | 72.5% |
| 0.05 mm | 0.444 | 73.7% |

At a shared 0.3 mm the transform was delivering under a tenth of what the technique can do. The budget itself was measured the same way, on the 180 mm cap, which is the case that cannot reach the floor:

| budget | mean \|rise\| | touched | peak RSS | time |
|---|---|---|---|---|
| 1M cells | 0.139 | 25.8% | 33 MiB | 3.7 s |
| **2M cells** | **0.186** | **32.7%** | **56 MiB** | **7.0 s** |
| 4M cells | 0.228 | 39.2% | 102 MiB | 13.6 s |

The memory column is what the budget buys and still stands. The time column was taken before [What a layer costs](#what-a-layer-costs) and is now about 2.3x pessimistic; it is kept because what it shows is the *shape* — cost is linear in the budget, quality is not.

**Do not fold this back into a shared constant, and do not let the wall-stacking test follow it down.** `footprint::CELL` is the tolerance of "these two beads overlap" and has its own measurement behind it: at 0.3 mm two beads sharing a cell overlap by more than half their width.

## The flow

Every stretch is metered for the gap it crosses. The layer under a surface is flat, so that gap is `height + rise` where the slicer metered `height`:

```
factor = (height + mean rise over the stretch) / height
```

Ironing is the exception: it runs over a surface that is already there, so it follows it in Z and is **not** re-metered. A bead that crosses a whole strip climbs from `-h/2` to `+h/2`, so what it gains at one end it gives back at the other and it comes out near enough as sliced — `audit.py flow` checks exactly that by undoing the gap and comparing with the input.

## The shape of the output

- **Sampled every half grid cell, written as few moves as possible.** The rise lives on the chosen grid and is box-blurred over it, so it holds no feature narrower than a cell and half a cell samples everything it can express — measured on a 60 mm cap against sampling at 0.02 mm, the written heights are identical at the 99th percentile and 1.3 µm apart at the 99.9th, under the 5 µm `TOLERANCE` a move is simplified to. **This was a dial (`--zaa-resolution`) and it did nothing**: across 0.05 mm to 1.0 mm the p99 difference was 0.00 µm and the output grew by 17%. It is stated as a share of a cell (`zaa::STEP` = 0.5) rather than in millimetres, because the cell is no longer the same on every file. A printer interpolates Z along a move, so a straight climb is one move however long it runs. `zaa::simplify` is a one-pass slope filter: it narrows the range of slopes a segment could still take as each sample arrives, and closes the segment when that range empties.

  **Its corridor is half `TOLERANCE`, and that is not a safety margin.** What gets printed is the chord from the anchor to the last sample that fitted, not the slope the corridor was kept for. An interior sample sits at most one corridor from that slope and never further along than the sample the chord ends at, so it is at most **two** corridors from the chord itself. Two halves are what make `TOLERANCE` describe the line that is printed rather than a line nobody prints. **The move counts and file-growth figures throughout this skill — 15824 in / 52113 out on the 180 mm cap, and the growth column of the table below — predate that change and have to be re-measured before they are quoted again.** A tighter corridor closes segments sooner, so it can only add moves; by how many is not known and is not to be guessed. The blur figures (54105 without it against 26610 with it) are a ratio taken on one build and still stand as a ratio.
- **No height change is ever a `G1 Z` of its own.** Same rule as bricking, same reason: a Z-only move names no other axis, so the planner stops the toolhead to run it. `Pass::drain` gives the height to the last move of the tail that can take it, and a line already carrying one of this tool's stamps still counts as free — bricking's own reset travel is usually the carrier.
- **Nor does one ever fall faster than a slope can — and a climb is bounded by something else entirely.** The surface is at its highest exactly where the layer above begins — that is what makes one layer's ramp meet the next one's — and a bead carrying on under the layer above has to be back on its plane, so the constraint has a half-layer step in it with one grid cell to do it in. Left raw that is not a path a nozzle can take: on the 60 mm cap **886 of 2207 written moves changed height faster than one in two**, the worst by 0.1 mm over 0.023 mm of travel. `Pass::ease` spreads the descent out ahead of the edge instead, at no more than one layer height per bead width — the steepest a surface can fall and still be a slope rather than a wall. It only ever **lowers** a sample and never touches a covered one, so a bead under the layer above still sits exactly on its plane and the visible wall keeps its ceiling. After it, all four real parts have **zero** moves steeper than one in two. Pinned by `zaa::tests::a_surface_never_drops_faster_than_a_slope_can`, which reads 4.000 against a limit of 0.444 with it switched off.

  A **climb** is held to one layer height per **grid cell**, not per bead width, because nothing is in its way. What bounds a descent is the nozzle's own flat underside plowing back through material it laid a bead width ago; climbing, it lifts away from that material into a gap the extrusion is already metered for. The only bound left is what the field can express, and the rise is box-blurred over a cell, so a cell is it. Held to the descent's figure instead it flattened a great deal: a bead leaving a covered stretch was kept low for a further bead width, and the far edge of a strip is exactly where the ramp has to reach half a layer for one layer's ramp to meet the next one's — so a tread narrower than a bead was levelled outright and the staircase came back.
- **Arcs are resampled into short straight moves, at a step taken from their own radius.** Upstream skips `G2`/`G3` entirely, which quietly does nothing on a file sliced with arc fitting on. `zaa::chord_of` is the longest chord that stays within `SAG` (1 µm, the coordinate grid) of the arc it spans — `2·sqrt(SAG·(2r − SAG))` — capped at the sampling step. So a 1 mm radius is sampled at 0.089 mm and a 10 mm one at whatever half a cell works out to on this file.
- **That same chord is the longest a written move may run, and it is the half that is easy to lose.** `simplify` judges a stretch by its *climb*, and the samples of an arc at a steady height sit on one straight climb exactly as readily as a straight move's do — the slope range says nothing about where a sample is in the plane. So it is passed the span as well as the tolerance. Without it, measured on a 1000-wall Benchy, **74 arcs came out as a single chord**: the worst was a 160° sweep of a 1.5 mm radius written as one straight move **1.27 mm inside its own wall**, which erased a whole 3 mm post on the stern deck. Landing on the circle is not the test — what is printed is the chord *between* two landings, so check its sagitta. Pinned by `zaa::tests::an_arc_across_a_surface_is_followed_round_its_own_curve`, which was passing on the broken build because it only checked the landings.

## Streaming

The surface of a layer is measured against the layer printed **over** it, which the pass writing the file has not reached. Keeping every layer's outline from the survey would answer that and would also make memory a function of how tall the print is — a bed-filling layer is a few hundred thousand cells and there is no bound on layers.

So `src/zaa/scout.rs` reads the file a **second time**, by a reader of its own that never gets more than a layer or two in front, and holds three layers. Peak RSS measured on a 22.8 MB, 180 mm part: 14.1 MiB for bricking alone, **57 MiB** with both; on a Benchy, 20.0 MiB with both. The extra is the surface grids for one layer, and it is bounded by neither the file nor the part — the grid is a fixed budget of cells, so a bigger part buys a coarser cell rather than more memory.

The two transforms compose in one pass: `zaa::Pass` is a `Write` that sits in front of the real one, so `brick::stream` writes into it and it writes the finished G-code out. They own different regions, so neither sees the other's work.

## What a layer costs

This is by far the most expensive thing the binary does — bricking a 925-layer, 18 MB duct takes 1.3 s and following its surface takes 15.0 s — and all of it is `surface::Builder::build`, run once a layer over a window of about a million cells. Where the time goes, measured on that file with a timer around each phase, before and after the work of 2026-08-14:

| phase | before | after | what changed |
|---|---|---|---|
| `mark` + `enclose` | 1.00 s | 0.97 s | — |
| `mouths` | 10.32 s | 4.75 s | no division per cell walked, then filled a run of a row at a time |
| the three distance transforms | 14.86 s | 6.36 s | a thread each |
| the rise, cell by cell | 4.06 s | 0.52 s | integer early-out, then bands on threads |
| `blur` + quantising | 2.48 s | 1.59 s | quantising folded in, and only the rows that hold a rise are walked |
| **whole run, wall clock** | **34.6 s** | **15.0 s** | |

Output was byte-identical at every step, on six files: `--zaa` and `--bricks --zaa` over that duct and over a Benchy sliced at 2 and at 1000 walls. **That is the only acceptable proof for a change here** — none of this is meant to move a single written height. The phase timings drift about 10% run to run, so a change worth under that has to be judged on wall clock over several runs.

Four things are worth knowing before touching any of it again:

- **Nothing here can be skipped by looking at the layer first.** The obvious wins are not there: on that duct **0 of 925 layers** took the vertical-face early-out, **0** had nothing exposed (`here.without(above)` empty), and 258 came out flat only *after* the whole window had been measured. Nor is the answer to stop each distance transform at the distance that matters: **57% of cells are within `carried` of this layer's own outline, 71% within a strip and a half of the layer below's, and 95% within `carried` of the layer above**, so a bounded wavefront would settle most of the window anyway and pay a queue for it. The reach is a slope rather than a length, and at 0.2 mm layers it is 11.5 mm.
- **The chamfer kernel is faster written as it is than hand-unrolled.** Replacing the closure with eight straight-line reads at fixed offsets from `at` — no casts, no comparisons, just the index arithmetic — came out **86% slower** (14.9 s to 27.8 s), and so did a version off three row slices, and putting the direction in a `const` parameter did not help, and neither did reducing the eight `min`s as a tree instead of a chain (6.4 s to 7.1 s). The four comparisons per neighbour let the compiler prove the index is in range and drop the bounds check with it. Do not "optimise" this loop again without measuring, and measure it against a whole real file.
- **`mouths` may take a neighbour as an index away, and fills a row at a time.** `window` keeps `MARGIN` cells clear and `enclose` floods that ring, so nothing hollow ever sits on the window's edge and every cell the walk reaches has all four neighbours; forcing that at the top of the function costs a row and a column of writes and saved 5.6 s of a 35 s run, because three quarters of a window is the inside of the part and a division was being paid for every cell of it. Filling a whole run of a row rather than a cell at a time took another 0.9 s. Which cells end up in `stack` is unchanged; only the order, which nothing downstream reads.
- **A thread has to be worth starting, and there is no pool.** Starting one costs about **19 µs** — measured, from the banded rise loop. The three distance transforms are one apiece and it pays, because each is milliseconds of work on a buffer no other touches. The blur is a third of that and does not: split fifteen ways it went from 2.5 s to **5.4 s**. `Builder::band` is the rule — whole rows, and one band under about a quarter of a million cells.

### What is left, and why it is left

The critical path is now one distance transform. Timed separately: **HERE 6.06 s, BELOW 6.06 s, ABOVE 2.43 s** — the two that measure a distance to the *outside* of a layer are the slow ones, because for a part that fills its own bounding box almost every cell is far from any source and so takes the whole kernel. Three lanes on sixteen cores, and the wall clock is the slowest lane.

So everything further needs a distance transform that splits *within* itself:

- **Caching it does not help.** Consecutive layers share a window 46% of the time on that duct, and BELOW at layer k is HERE at layer k−1, so it could be kept. But the wall clock is `max(HERE, ABOVE, BELOW)` and HERE alone is the whole of it.
- **A separable exact Euclidean transform would split**, one band a thread, and would be more accurate than a 5-7-11 chamfer rather than less. What stops it is that both of its passes take a different partition of the array — rows then columns — and the only way to hand a thread a borrowed band is `std::thread::scope`, which spawns. At 19 µs a spawn, banding both passes of three transforms over 925 layers is **1.3 s of spawning alone**, against about 4 s of work saved. A persistent pool would fix that and needs either a dependency or `unsafe`; neither is on the table here.

## Verifying a change

```sh
cargo run --release -- --zaa -v -o /tmp/out.gcode part.gcode
python3 .github/skills/zaa/scripts/audit.py surface   part.gcode /tmp/out.gcode
python3 .github/skills/zaa/scripts/audit.py invariant part.gcode /tmp/out.gcode
python3 .github/skills/zaa/scripts/audit.py flow      part.gcode /tmp/out.gcode
python3 .github/skills/zaa/scripts/audit.py cover     part.gcode

# And bricking's own invariant over the COMPOSED output, with the original
# passed so the plane comes from a file that still sits on one.
cargo run --release -- --bricks --zaa -o /tmp/both.gcode part.gcode
python3 .github/skills/bricklayers/scripts/audit.py invariant /tmp/both.gcode part.gcode
```

- `surface` — what moved and by how much, and that nothing moved which is neither a surface nor the wall that shows. Only meaningful when the input has not been bricked first.
- `invariant` — nothing below the bed, no negative extrusion, nothing more than half a layer off its own plane.
- `flow` — undoing each stretch's gap has to recover what the slicer metered. Measured on real output: **0.05%** on a 60 mm cap and **0.03%** on a 1.9° cone, where one move spans far more of the ramp.
- `cover` — how much of a file is a region this can touch at all, which is what says whether a file is a useful test at all.

A part with no shallow surface has to come back **byte-identical**. A plain cube and a straight cylinder both do.

### Getting a test part

A Benchy is a poor subject: it is mostly steep. Slice a shallow spherical cap instead — the slope sweeps from flat at the apex to steep at the rim, so one part exercises the whole range. OrcaSlicer slices headless, but the machine and the process profile have to match each other:

```sh
D=~/.config/OrcaSlicer/system/BBL
orca-slicer --datadir ~/.config/OrcaSlicer \
  --load-settings "$D/machine/Bambu Lab P1P 0.4 nozzle.json;$D/process/0.20mm Standard @BBL P1P.json" \
  --load-filaments "$D/filament/Bambu PLA Dynamic @BBL P1P.json" \
  --slice 0 --outputdir /tmp part.stl
```

## Real-file measurements

OrcaSlicer 2.4.2, 0.2 mm layers, 0.4 mm nozzle, run with `--zaa` alone so the figures are this transform's and nothing else's. Nothing is passed on the command line — there is nothing to pass.

| part | followed | of their plane | written as | growth |
|---|---|---|---|---|
| 60 mm spherical cap | 2678 moves, 17 of 90 layers | −0.074 to +0.100 mm | 6243 moves | 1.096× |
| 180 mm spherical cap | 15824 moves, 52 of 240 layers | −0.082 to +0.100 mm | 52113 moves | 1.085× |
| 60 mm cone, 1.9° | 1712 moves, 5 of 20 layers | −0.091 to +0.100 mm | 3318 moves | 1.175× |
| Benchy, 2 walls | 1608 moves, 86 of 240 layers | −0.081 to +0.100 mm | 5557 moves | 1.076× |
| Cube, flat plate with a boss | nothing — every face is vertical or flat | — | — | 1.000× |

**The last two columns are stale and the first three are not.** `simplify`'s corridor has since been halved, which only ever closes a segment sooner, so the moves written and the growth can only have gone up from what is tabulated. What was *followed*, on which layers, and how far off plane it went are properties of the field rather than of the simplifier, and those columns still stand. Re-measure the two before quoting them; do not scale them.

**A minority of layers is the right answer.** A staircase only shows where a layer leaves a tread wider than the bead standing on it; 19 of the 60 mm cap's 90 layers do, and 17 of those are followed. Weighted over those layers alone the surface comes out **0.324 of half a layer** off the plane across 56.2% of their surface path — that is the share of a removable step actually removed, and it is the number to quote when judging a change. `scripts/audit.py` does not compute it; the metric is "weight `|z - plane| / (height/2)` by path length over the wall and top-surface path of layers whose outer-wall extent shrinks by at least a bead".

**A Benchy scores 0.029 over 8.8% on that same metric and it is mostly the part's doing.** Its hull flares outward, so nearly every face is an overhang, a vertical wall or a flat deck, and 18 of its 241 layers leave a tread against 18 of the cap's 91. A user reporting "`--zaa` changed nothing on my Benchy" is largely describing the part — but measure before saying so, because that figure was 0.015 over 5.0% until the move-by-move coverage veto came out.


Per region on the 60 mm cap, from `audit.py surface`:

| region | moves | lowest | highest | mean |
|---|---|---|---|---|
| hidden wall | 2622 | −0.033 | +0.100 | +0.046 |
| visible wall | 902 | −0.074 | −0.001 | −0.027 |
| top surface | 602 | −0.019 | +0.100 | +0.078 |

The asymmetry is real and it is the geometry: the visible wall sits at the outer edge of the strip where the surface is low, and everything behind it fills the rest of the ramp. The visible wall's upper bound is at most zero because it is held there — see [the visible wall is never raised](#the-visible-wall-is-never-raised).

`audit.py flow` on that output: 0.05% on the 60 mm cap, 0.03% on the 180 mm one and 0.03% on the 1.9° cone.

Time and memory on a 22.8 MB bed-scale cap: bricking 0.16 s at 14.1 MiB peak, both transforms 7.0 s at 57 MiB.

### The visible wall is never raised

`zaa::Pass::follow` gives the visible wall a ceiling of zero: it may be lowered to follow the surface and never lifted. Two reasons, and both matter.

- It is bricking's own invariant, and holding it here means it holds for the whole binary whichever transforms are run — `.github/skills/bricklayers/scripts/audit.py invariant` reports 0 of 13977 on the composed output of the 60 mm cap.
- A bead of the visible wall standing proud is out of reach of the nozzle's flat underside, so what would be ironed level is free to bulge, and it does it where it shows. Upstream reached the same rule from the seam it produced.

The cost is small because of where the wall sits: its centreline is about a fifth of a millimetre inside the outline, so the surface there is below the plane unless the tread is down to about a bead. Where it is, the cap holds the wall still and the surface behind it does the climbing. Pinned by `zaa::tests::the_wall_that_shows_is_never_taken_above_its_plane`, over five layer heights — which is what sweeps the reach now that it is derived from one.

## Settings

**There are none.** `--zaa` turns the transform on and that is the whole of it; it combines with `--bricks`, and naming neither is refused.

| what was a dial | where it comes from now |
|---|---|
| how wide a step to follow | `zaa::reach_for(height)` = `height / tan(SHALLOWEST_SLOPE)`, per layer. `SHALLOWEST_SLOPE` is 1°, so 11.5 mm at 0.2 mm layers and 4.6 mm at 0.08 mm ones. |
| how finely to sample one | `zaa::STEP` = 0.5, a **share of a grid cell** rather than a distance. An arc takes the finer of that and `chord_of(radius)`. |
| how fine the grid is | `Grid::for_span`, from the part's own span against a fixed budget of cells. |

`Config` carries `layer_height`, `wall_width` and `bricked`. The first two exist for library callers and are filled from the same sources bricking uses, so an adaptive slice is measured against each layer's own height; `bricked` is `bricks || survey.bricked`, so a file already carrying this tool's own brick stamps locks the hidden wall even on a `--zaa`-only run.

### Why the reach had to stop being a number in millimetres

Measured before the change, on real slices at the old default of 4 mm:

| part | at 4 mm | at 8–50 mm |
|---|---|---|
| 60 mm cap (3.8° and steeper) | 3531 moves | **identical** — byte for byte from 3 mm to 50 mm |
| 1.9° cone, 6 mm treads | **nothing followed** | 698 moves on 4 layers |
| plain cube, flat top | untouched | untouched |
| flat plate with a boss on it | untouched | untouched |

So the fixed default was refusing a 1.9° slope — a staircase at its most visible — while a reach twelve times wider cost nothing in time or peak memory and left a flat top alone. **What protects a flat top and a ledge is `sloped`, the layer-below test, not the reach.** A cube and a plate-with-a-boss are byte-identical at every reach up to 50 mm.

The reach still has to exist, and it is not a preference. A covered cell measures `up = 0`, so its `strip` is its own distance to the outside of its layer: a cell just inside the strip's high edge measures the strip itself and correctly continues the ramp to `+0.5`, while a cell deep inside the part measures a strip as wide as the part. `fading` on the strip's width against the reach is what tells those two apart. What was wrong was stating the bound in millimetres: a step's width is a slope, and a slope is `height / tan`, so the same 4 mm meant 2.9° at 0.2 mm layers and 1.1° at 0.08 mm ones.

## What upstream does that this does not

Both upstream implementations have the mesh, so they know the surface exactly rather than inferring it, and both apply the same `±height/2` clamp — which is not a coincidence, it is the range the exposed strip spans under mid-layer slicing.

- `zaa_minimize_perimeter_height` (adob) lowers the visible wall by a further half its own width along the slope. Not done here.
- `zaa_dont_alternate_fill_direction` (adob) is a slicer setting; a post-processor cannot change the fill direction.
- Hidden walls are contoured upstream, and they are here too when `--bricks` is not running alongside. See [Why the visible wall is always safe](#why-the-visible-wall-is-always-safe-and-a-hidden-one-only-sometimes).
- Arcs are skipped upstream and resampled here.
- Upstream also refuses to raise an external perimeter, and reached that rule from the seam it produced rather than from the invariant. Same answer, and it is enforced here too.
