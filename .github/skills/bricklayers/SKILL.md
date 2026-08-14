---
name: bricklayers
description: Domain knowledge for corbel's BrickLayers transform — how brick layering works geometrically, how perimeter loops are grouped into contours, which signals in sliced G-code are trustworthy and which are traps, real-file measurements, and an audit script for verifying output. Use when changing src/brick.rs, src/scan.rs or src/gcode/feature.rs, when a user reports that bricking looks wrong or does nothing, when reasoning about loop detection, contour boundaries, raise parity, extrusion factors, arcs, wall order, or slicer dialects, and before trusting any claim about what a slicer emits.
---

# BrickLayers

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
2. **The multiplier is a flow for the walls — the visible one included — not
   compensation owed to
   the raise.** A raised column has the same spacing as an unraised one, so
   volume conservation alone gives `1.0` and the flow at the two ends of a
   column is derived separately. `Config::wall_flow` sits on top of that and
   scales **every internal perimeter bead, raised or not**, so a climbing bead
   takes `1.25 × m` and a capped one `0.5 × m`.

   **`m` is read off the file, per layer** (`brick::automatic_flow`), and the
   only dial over it is `--extra-flow`, a percentage naming the extra a wall
   takes **where the layer is as thick as the nozzle**. It sets the slope and
   the geometry decides where on it each layer sits, which is what keeps the
   per-layer derivation: an absolute pin is a constant and the flow is not, so
   on an adaptive slice one number is wrong nearly everywhere.
   `Config::wall_flow` still pins an absolute value, for tests and library
   callers only. Slicers do not space
   beads at the nominal width: `Flow::spacing()` is `w - h(1 - π/4)`, at which
   spacing a stadium bead's area is exactly `h × spacing`, so the slab is
   filled with **zero net void** and there is nothing to fill. Measured on
   three real Orca slices (0.4 nozzle, 0.2 layer, 0.45 inner / 0.42 outer,
   `filament_flow_ratio 0.95`): metered inner bead area 0.0773–0.0774 mm²
   against the formula's 0.0774, outer 0.0716–0.0717 against 0.0716, and
   neighbouring loops running 0.4074 mm apart against the formula's 0.4071.
   **A model that spaces at the nominal width predicts 0.0855 — 10.5% out.**

   What *is* geometry is the corner two beads leave between them: it is `h`
   tall and closes as they are pushed together, so the share of a bead sitting
   in one is proportional to `h / spacing`. Against a flat plane the nozzle
   presses those closed; over a staggered seam half of each is out of reach.
   So `m = 1 + extra × (H_REF/NOZZLE_REF) × (h/s) / (H_REF/S_REF)`, held to
   `flow_ceiling(h, s)` = `2 - h(1-π/4)/s` and floored at 1, where `extra` is
   `--extra-flow` as a fraction. That ceiling is the flow at which the bead's
   own edge reaches the centre of the loop beside it, so it is the bead model's
   arithmetic rather than a number chosen to look safe; it is ~1.89 on the
   reference profile and binds on **no** geometry a slicer produces (nozzles
   0.2–1.2 mm, widths to 1.2× nozzle, layers a tenth to four fifths of it —
   pinned by `the_flow_ceiling_never_binds_on_a_geometry_a_slicer_produces`).
   The
   anchor is `REFERENCE_NOZZLE` 0.4, `REFERENCE_HEIGHT` 0.2 and
   `REFERENCE_WIDTH` 0.45 — the profile every measurement here was taken on,
   and one whose layer is exactly half its nozzle, so it takes half the slope.
   At `DEFAULT_EXTRA_FLOW` (0.05) that is 1.025; 0.1 mm at 0.45 gives 1.012,
   0.28 mm gives 1.037, 0.2 mm at 0.35 gives 1.033, 0.15 mm at 0.85 gives
   1.009. **Every one of those appears in the README's tables — re-check all of
   them before touching any constant.**

   **Do NOT re-derive this from `void% = 21.46 × h/w`.** That number is
   `1 - π/4` and it is the void left when beads are spaced at the nominal
   width, which is a slicer that does not exist; it was measured and refuted
   here twice, once in 2026-08-03 and again in 2026-08-11. The scale of the
   multiplier is a physical constant that has been printed with, not a volume
   deficit; only its *shape* is geometry.

   The width comes from `perimeter_extrusion_width` (Prusa/Super) or
   `inner_wall_line_width` (Orca/Bambu), out of the file's settings block, the
   `SLIC3R_*` environment, or a bgcode container's metadata — **and a
   percentage is resolved against `nozzle_diameter`**, which the same three
   sources state. Both real Prusa bgcode fixtures carry both keys.

   The layer on the plate is excluded, for the reason in point 3 — nothing
   presses a bead there. Nothing outside a perimeter region is re-metered:
   only those regions are buffered for flow, and only a buffered line is ever
   rescaled. Verified on a 40-layer three-wall part with a hole: infill and
   solid surface `E` words byte-identical to the input (240 occurrences
   unchanged), internal wall `E` words rewritten everywhere but the bed layer
   (320 → 8, 1280 → 32). Cost on that part at `1.02`: 1.53% of its mass, where
   the same figure on a slicer's global flow ratio would add 2%. `--verbose`
   prints the exact figure per file.

   **The joint against the visible wall is fed from one side only** unless the
   visible wall is scaled too, and scaling it alone would grow the part: a
   0.45 × 0.2 bead is 0.0814 mm², so +2% is ~8 µm of outward growth per wall
   and +30% is ~122 µm. So the visible wall gets **both**: the same multiplier
   as every other wall, and a move inward of `(m − 1) / 2 × width`, which is
   half the width it gains. A bead widens about its own centre, so half of the
   gain going outward exactly cancels the move and the outer face does not
   shift. Verified on a 100 mm box at 1.02: outside face to face 100.010 mm
   before and after, to within the 1 µm G-code coordinates can express.

   Do NOT move it without scaling it, or scale it without moving it — the first
   shrinks the part by the offset, the second grows it by twice the offset. And
   do NOT scale it by more than `m`: the joint then receives 3 units where every
   other joint receives 2.

   The offset itself is `src/geometry/inset.rs` and it always goes **left of the
   direction of travel**: slicers emit an island anticlockwise and a hole
   clockwise, so left is the material side of both and nothing has to work out
   which it was handed. `Pass::flush_skin` buffers the region, `Pass::move_skin`
   groups it into closed loops, and `E` carries `ratio × m`, where the ratio is
   the new path length over the old. It is `Pass::skin_offset()`, a method
   rather than a field, because `m` now moves with the layer: pinning it once
   per file would leave the offset and the flow disagreeing on every layer of
   an adaptive slice.

   **An arc moves with the rest of the loop, and `inset::Edge` is how.**
   `offset(points, edges, delta)` takes an edge per vertex — `Edge::Straight`
   or `Edge::Arc { centre, clockwise }` — and a vertex is placed at the miter
   of the two *tangents* meeting there, an arc's tangent being at right angles
   to its radius. The arc then keeps its centre and changes radius
   (`r - delta` anticlockwise, `r + delta` clockwise, i.e. into the material
   either way), and `brick` restates `I`/`J` from the moved start point. A
   vertex an arc touches is pulled onto that arc's offset circle, because the
   radius a printer sweeps is read off the arc's own start point: a start a few
   nanometres out is drawn at the wrong radius all the way round, while the
   straight move after it can start anywhere. `keeps_its_arcs` rejects a loop
   whose arcs would end more than `ARC_SLACK` (0.01 mm) off their own radius,
   which is what two arcs meeting at a sharp corner ask for.

   **Do NOT go back to declining arcs.** It was the single reason the README's
   "your dimensions are kept" was false in practice: measured before the fix,
   429 of a Benchy's ~800 visible loops and **all 126 of a solid cylinder's**
   were declined for holding an arc, so the cylinder had **0 of 684** visible
   beads moved and grew by half of what every bead gained. After: **21208 of
   21396 (99.1%) on the Benchy and 674 of 684 (98.5%) on the cylinder**, with
   185 and 5 of the remainder being the layer on the build plate and 3 and 5
   the loops the slack guard turned down. Every bead already moved before the
   change moves to exactly the same place (14476 of them, 0 different, 0 lost),
   every commanded Z is unchanged, parity and the invariant are unchanged, and
   the retract/prime stream is identical.

   **Do NOT re-derive the arc offset as a change of centre.** Solving for a
   centre that keeps both moved endpoints at the wanted radius is ill
   conditioned at both ends of the range — a near-full-circle arc has its two
   endpoints nearly coincident, so the perpendicular bisector they define is
   nearly undefined, and a half-circle puts the solution's square root near
   zero. Keeping the centre is stable everywhere, and measured on real files
   the mitered start lands on the offset circle to p99 = 0.00 µm.

   **A moved travel must still be able to carry a height change.** The travel
   that reaches a loop is both taken sideways and given the loop's height, on
   one line, through `Line::write_moved`'s `z` argument. Excluding moved lines
   from `Pass::carrier` instead — which is what the code did — puts back the
   standalone `G1 Z` that stops the toolhead on the seam: measured on a Benchy,
   1772 halting Z moves became 2013 once arc loops started moving, and 1510
   once the carrier could do both.

   **The width behind the offset is never absent.** `Pass.skin_width` is an
   `f64`, not an `Option`: it takes
   `external_perimeter_extrusion_width`/`outer_wall_line_width` where the file
   states one (a percentage resolves against `nozzle_diameter`), then the
   hidden walls' width, then `REFERENCE_WIDTH`. It used to be `None` on a file
   that stated nothing — a Cura file — and the offset was then zero while the
   flow still fell back to the reference profile, which is precisely the
   "scaled but not moved" case that grows the part. The two halves of the
   change fall back together or not at all. Files that do state a width are
   byte-identical across this change.

   What still declines and passes the loop through as sliced: an open fragment,
   fewer than three beads, a flow of 1.0, a loop whose arcs will not survive
   the move, and the layer laid on the build plate.

   **A ring does NOT return to where it started, and demanding that it does
   switched the whole feature off on every real file.** A slicer stops the last
   bead short so the two ends do not pile up at the seam. Instrumented over the
   loops `move_walls` actually considers on two real OrcaSlicer files: all 308
   of them land **0.0385 to 0.0411 mm** short — the `seam_gap` default, a tenth
   of a 0.4 mm nozzle — and **not one closed to the 1e-6 mm the test demanded**.
   The visible wall was therefore scaled and never moved, which is exactly the
   case the rule above says grows the part. The tolerance is now one stated bead
   width, ten times the observed gap; measured effect on a real Benchy:
   **0 → 14452 of 21395 outer beads moved**, median 0.0042 mm, which is
   `(m − 1) / 2 × 0.42` to the micron. (Those counts predate arcs being moved;
   the same Benchy is now at 21208 of 21396.)

   **The closing vertex has to carry the start's move, not its own normal.** It
   sits mid-edge while the loop's start is a corner, so offsetting each by its
   own geometry shrinks the gap by the whole offset (0.040 → 0.036 on a
   fixture), and at a wide enough offset it runs the bead **past** its own seam
   into a double bead. `offset[last] = offset[0] + (closes − entry)` keeps the
   gap the slicer chose, exactly: 0 of 1864 real loops had theirs changed. That
   override can pull a closing **arc** off its own circle, so `move_walls` asks
   `keeps_its_arcs` again after applying it.

   Verified on a real Benchy and a solid cylinder at the 5% default: the
   visible wall moves 0.0041 to 0.0071 mm (the offset, plus the reach a miter
   legitimately adds at a corner), 20925 beads to the left of travel against 1
   to the right, and flow per mm against the input at 1.0249 for the median
   bead. The spread — p1 1.0167, p99 1.0301 — is three-decimal coordinates on
   short beads, not metering. Nothing but the visible wall moves: on the Benchy
   exactly 156 further beads move and every one is labelled `Overhang wall`
   inside a loop that also carries `Outer wall`, which is the visible wall.
   Infill and surfaces ×1.0000; audit invariant 0.

   **`flush_skin` must hand its trailing lines back to `keep`.** The travel out
   of a wall is emitted before the next region's marker, and it is what the next
   raise rides; draining it with the region would leave that raise a bare
   `G1 Z` on the seam. Measured on the same file with the offset dormant and
   active: 4 bare-Z stops and 4 raises riding a travel, both ways.

   Do NOT try to solve the joint with per-position multipliers instead. Pinning
   the skin at zero and asking every joint to receive the same fill inverts to
   `0, 2T, 0, 2T…` — alternate walls at double and the ones between them at
   nothing, which is just "scale the raised loops only" at twice the rate.
   There is no uniform-plus-edge-correction solution to those rules.

   **The wall facing the infill is NOT moved outward by the mirror of this, and
   the reason is the slack that is already there.** The arithmetic is
   symmetric — offset the innermost loop by the same `skin_offset()` with the
   sign flipped and its inner face lands exactly where the slicer drew it — but
   the two faces are not comparable. The visible wall's outer face has no
   slack: it is the part's dimension. The innermost wall's inner face has slack
   the slicer chose. Measured on a stock 2-wall OrcaSlicer Benchy
   (`target/tmp/infill_gap.py`, nearest separately-metered material to every
   internal-wall sample, per layer): the gap peaks hard at **0.34 to 0.36 mm**
   against the 0.39 to 0.41 mm two beads touching would give, i.e. the slicer
   already runs infill **0.04 to 0.06 mm inside the wall's centreline** — the
   file's own `infill_wall_overlap = 15%`. What the mirror move would correct
   is `(flow − 1)/2 × spacing` = **5.1 µm** at the derived 1.025, a tenth of a
   slack that is a dial on the slicer's own page.

   It is also not reliably the loop that faces infill. On that same Benchy
   **9.3% of internal wall path has no separately-metered material within
   2 mm** of it — the hull is thin enough there that two wall bands meet — and
   on the 1000-wall version it is 56.6%. Those loops face a staggered
   wall-to-wall joint, and pinning both edges of a band is exactly what starves
   one. Do not reopen this without a sectioned print.

   **None of this is measured yet.** Whether a void opens at the staggered
   joint, and how big it is, has never been checked against a sectioned print.
   The offset is a defensible construction, not a finding. Anything built on
   top of it inherits that.
3. **The bed layer is never raised, and a column climbs over the two layers
   above it.** Displacing a column upwards opens a half-layer void under its
   bottom bead that has to be extruded once, whichever layer starts it. Asking
   one bead for all of it leaves the nozzle half a layer clear of the surface
   it lays against, so it presses nothing and the surplus spreads sideways —
   on a Benchy that filled in the bottom nameplate, which is exactly one layer
   deep. `RAMP` (2) spreads it: no bead spans more than a quarter of a layer
   beyond what the slicer metered it for, and the layer on the plate comes
   through byte for byte.
4. **One formula covers all of it.** `Pass::geometry` is
   `(layer_height + rise(here) - rise(under)) / layer_height`, where `rise` is
   the column's offset. It falls out at 1.0 on the bed
   (nothing has risen yet), 1.25 on each climbing layer, 1.0 once the column is
   up — which is where the wall flow applies instead — and 0.5 where a cap
   forces the offset back to zero. A column capped before it
   finished climbing gives back only what it took, e.g. 0.75.
5. **Both ends of that span are MEASURED, never inferred from the parity.** A
   loop's parity is not its column's history: numbering runs from the visible
   wall inward, so a wall that gains or loses a loop renumbers, and a flaring
   hull does that every few layers. The loop then laid on the plane can sit
   directly over a bead standing half a layer proud — metered for a whole layer
   it pours **exactly twice** what the gap holds, which is a blob on an
   overhanging surface. Measured on a stock 2-wall Benchy, **188 mm of internal
   wall path at exactly 2.00×**, most of it between Z8 and Z15 where the hull
   flares hardest; 2575 mm on the 1000-wall version. So `Pass.rising` collects
   the cells each layer leaves standing proud, `Pass.standing` is the layer
   below's, and `Loop.on_a_raise` tests a loop's own path against it.
   `SEAM_SHARE` (0.25) is the threshold and it is **not** `CAP_SHARE`: capping
   asks whether a column *ends*, where being wrong costs a void, and this asks
   what a bead *sits on*, where being wrong the same way costs a blob, so it is
   biased the other way. It can be, because the two populations barely touch —
   a loop laid on the plane shares a tenth of its path or less with the raise
   below, a loop carrying a raised column on shares 0.4 to 1.0, and the valley
   between is empty. `CAP_SHARE` cuts straight through the upper population.
   Record the cells during the walk `shares` already makes: walking a second
   time for them cost +22% of `brick` where folding them in costs +16%.
6. **A column has to be capped wherever it ends, not only at the top of the
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
7. **`layer_height` in that formula is the layer's OWN height, and so is the
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

Two loops are the same wall when **most of one's path runs within
`MAX_LOOP_GAP` (2.0 mm) of the other's** — `Pass::adjacent` in `src/brick.rs`.
A bounding-box comparison rejects most pairs first; then up to `PROBES` (16)
sampled points of one loop are tested against every point of the other, with an
early exit as soon as neither the probes left nor the ones already counted can
change the answer.

**Coming close somewhere is not enough, and asking only that was a bug.**
`BESIDE_SHARE` (0.5) is how much of the probed path has to run within the gap,
and it is measured **both ways round** because a wall that widens leaves the
shorter loop beside the longer along the whole of itself while the longer runs
on past its end. A second wall passing close by is not like that: a 5 mm hole
1.2 mm from a straight wall has a quarter of its path within the gap, and on
the "anywhere" test that pulled the hole into the island's contour, where its
loops were ordered by their distance to the island's visible wall — a distance
that drifts layer to layer on anything tapered or curved, so the hole's stagger
flipped from one layer to the next and the hole's own visible wall sat in a
contour of its own that is never raised.

**A loop is probed along its path, not at its endpoints.** A `G2`/`G3` states
only where it ends, so two concentric arc-fitted loops a bead apart whose
slicer cut them at different angles measure a chord's bulge apart rather than a
bead: at a 10 mm radius a 45° disagreement puts their endpoints 7.5 mm from
each other, far past `MAX_LOOP_GAP`, so every loop of a curved wall became a
contour of its own and the whole stagger was lost — which is every curved wall
a slicer with arc fitting on emits, and that is the Bambu Studio default.
`Pass::points` walks an arc round at `ARC_STEP` (`MAX_LOOP_GAP / 4`), which
leaves the worst sampling error an eighth of the gap being tested and is about
the spacing a slicer puts between the vertices of a loop it did *not* fit an
arc to.

Measured over five prints: neighbouring loops of one wall run 0.4–1.5 mm apart,
the next island is more than 3 mm away, and almost nothing falls in between.

### What does not work — all three were tried and shipped before being caught

| Signal | Why it fails |
|---|---|
| **Retraction before the loop** | Slicers retract *between neighbouring loops of one wall* whenever the seams are far apart, and cross to another island *without* retracting whenever the travel is short. Mis-grouped ~2600 transitions per print. |
| **Distance from one loop's end to the next one's start** | Noise. Spreads evenly from 0.5 mm to past 10 mm, because the seam can sit anywhere around a loop. |
| **Bounding-box nesting (does one loop enclose the other?)** | Most of a wall is not a closed ring. Where the slicer follows a curve the loops are arcs offset *sideways*, each often longer than the last, so neither encloses the other. Dropped a 1000-wall Benchy from 1392 raised loops to 314. |

**An overhang names a condition, not a wall.** `; FEATURE: Overhang wall` is
emitted *in place of* the wall's own label wherever it runs over air, and
OrcaSlicer 2.4.2 interrupts an **inner** wall with it mid-loop, with no travel
between the two labels. It used to classify as `ExternalPerimeter`, which since
the visible wall became the anchor meant "this loop is the outer one" — so a
loop that merely began over air anchored its contour and pushed the real outer
wall into a contour of its own. It is now `Feature::Overhang`: a perimeter, so
it is buffered and numbered with the stack, but never an anchor. A loop's wall
is whatever *any bead of it* was labelled, recorded in `buffer()` at the bead
rather than at the marker — testing at the marker flags the loop that just
*ended*, since the next loop's travel has not arrived yet. A loop labelled only
overhang is held flat: ground truth (the same model with overhang detection
off) says **83.7% of overhang extrusion is really the visible wall**, so
raising it on that evidence puts a step on the surface five times out of six,
and it is 0.08% of a print.

`scripts/gcode.py` must match: it classified `overhang` as external too, which
made every raised inner wall look like an invariant violation — 51 reported on
a file whose real count is 0. Its positive control still fires; check it after
touching `classify`.

### Numbering direction

Which loop is number one decides which loops are raised, so it must be a loop
that stays put. **A wall gains and loses loops at its hidden end as it
thickens**, so numbering from the first-printed loop inverts the stagger every
time the count changes — every third layer or so on a Benchy hull.

**The visible wall is the anchor, and it is found by identity, not by
position.** `number_loops` looks for the loop whose `Loop::external` is set and
numbers outward from it; phase zero is that loop and is flat, so the
alternation runs inward through the whole stack. Three loops leave both ends
flat and raise the one between them; four raise the far end as well. External
perimeters are buffered alongside internal ones for this — `feed` keeps the
region open across a perimeter-to-perimeter marker so both halves of a wall
land in one contour and get numbered together.

**Print order is not stack order, and `inner-outer-inner` proves it.** Orca's
sandwich sequence prints the wall's outer half, then the visible wall, then the
innermost loop — so the loop *printed* one step from the anchor sits a whole
stack away from it *geometrically*. Numbering by buffer position put two
neighbours on the same level, which is exactly the seam bricking exists to
remove. Measured before the fix on a synthetic 3/4/5-wall stack: broken at 3
and 5 walls, correct at 4, because the parities only agree when the count is
even. Two things were needed, and both are load-bearing:

- `number_loops` ranks a contour's loops by `Pass::gap` — the closest their
  paths come to the anchor — whenever the anchor is *not* at either end of the
  contour. Where it is at an end the sequence was monotonic and buffer position
  is already the answer, so `inner,outer` and `outer,inner` pay nothing.
- `assign_contours` tests a new loop against **every loop of the open contour**,
  not just the one printed before it. The loop after the anchor in a sandwich
  is the innermost one, which on a wall thicker than `MAX_LOOP_GAP` is further
  from the anchor than any two neighbours ever are; comparing only against the
  previous loop split it into a contour of its own and numbered it from
  scratch. This broke at 7 walls and above.

Verified across all three sequences at 2 to 9 walls: identical output, no two
neighbouring walls on the same level anywhere. Throughput unchanged at
320 MB/s.

**Test fixtures need realistic point density for any of this.** A 30 mm square
emitted as 4 vertices puts its nearest *vertex* 2.55 mm from a loop whose path
runs 1.8 mm away, because `adjacent` and `gap` compare vertices rather than
segments. That reads as a contour split and looks exactly like a real bug — it
cost a wrong diagnosis here. Walk a fixture's loops in ~2 mm steps.

Because the anchor is recognised rather than counted from an end, **the wall
order no longer changes the result wherever a contour holds a visible wall.**
`Config::external_perimeters_first` survives only as the fallback for a contour
that holds none, which is what a hole's loops look like when the slicer split
them across regions. Wall order still cannot be read from the file's geometry:
marker-transition counting gave 1066 vs 1042 on one file (a coin flip), and the
geometric direction is genuinely mixed within a single file because holes print
opposite to island walls. Take it from the slicer environment
(`SLIC3R_EXTERNAL_PERIMETERS_FIRST`, `SLIC3R_WALL_SEQUENCE`) or from the file's
own config block (`; wall_sequence = ...`). There is no flag for it — a
`--wall-order` override existed and was removed.

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

### Fillers: gap fill and thin walls

`brick::is_filler` covers `Feature::GapFill` and `Feature::ThinWall`, and they
are **buffered with the wall they interrupt but are not loops of it**.

- **Buffered, never written straight out.** Both arrive in the middle of a
  wall, so a filler emitted as it arrives lands ahead of loops the slicer laid
  before it, and the loops either side of it fall into different contours.
  `Pass::fills_a_buffered_wall` is the gate: a filler joins the buffer only
  where there is a wall in it to keep whole.
- **Never raised, and never taking a place in the alternation.** A thin wall is
  what a wall becomes where it narrows to under two beads, so **both** its
  faces are the visible one — half a layer of step on it is half a layer of
  step on the outside of the part. Gap fill has no column of its own to
  stagger.
- **They are metered differently, and that is deliberate.** Gap fill is laid
  down into the valley *between* two beads and straddles whatever those two
  did, so it comes out **exactly as the slicer wrote it** — factor 1.0, no wall
  flow. A thin wall is a bead of its own with a plane under it, so it takes
  `Pass::geometry` like any other bead: where that plane is a column standing
  half a layer proud it has half the gap to fill. That is also the one reason a
  filler may count as covering a raise beneath it.

So "gap fill comes out as sliced" is true of gap fill and **not** of a thin
wall. Both are `is_filler`; only one of them is metered as sliced.

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
- **The survey and the rewrite must agree on what extrudes, and they agree by
  asking one predicate.** The rewrite asks about cells the survey drew, so a
  bead one of them counts and the other does not is a cell asked about that was
  never drawn. Both call `Line::draws_in_plane`: a move that names `X` or `Y`,
  or an arc, which sweeps through the plane whether or not it says where it
  ends. Demanding **both** `X` and `Y` reads a bead running along one axis as a
  travel, and demanding either of them reads an arc that names only `I`/`J` — a
  full circle — as nothing at all. Whether material actually came out is the
  caller's own question, since only the caller knows what the extruder has been
  told since; the same predicate is also what lays out a file that states no
  layers of its own, and there the two passes have to agree on which bead
  opened which layer or every per-layer set is consulted for the wrong layer.
- **A walk that cannot be finished is a REFUSAL, not a short answer.**
  `footprint::Trace` is what a walk returns, and `Trace::Refused` has to reach
  the caller. Everything downstream reads a missing cell as "nothing was
  printed there", so a trace cut short in the middle is a lie no caller can see
  through — here it is a column capped where it carries on — where a refusal is
  one every caller can act on. A refusal keeps **only** the two cells the
  move's ends stand in, because that is where the nozzle demonstrably was and a
  caller sizing a window off the footprint has to see them; cutting the walk
  short at `MAX_CELLS` instead left a trail heading off toward a coordinate no
  printer could reach. `brick` leaves the layers holding such a move unbricked
  and warns; no printer makes that move, so it is never a hard failure
  (`brick::tests::a_layer_holding_a_move_that_cannot_be_followed_is_left_unbricked`).
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
  `Pass::keep` holds a tail back across the marker and lets the first loop's lead
  reach into it. Without that tail every region's first raised loop needs a stop
  of its own: 23 of them on the same file, 1805 on a 3h print.
- **The tail must hold EVERYTHING that lays no bead, not just moves and
  comments.** A slicer drops progress, fan, acceleration, tool and origin codes
  between the layer's `G1 Z` and the wall that follows it, and draining the tail
  on one of those writes the travel out before the raise can ride it. Measured
  on a stock OrcaSlicer 2.4.2 file that puts `M73` there twice: 2 of 132 raises
  fell back to a bare `G1 Z`, and **132 of 132** once an `M73` followed every
  layer's `G1 Z` — the same for `M106`, `M204`, `M104`, `M400`, `T0` and
  `G92 E0`. Cura writes a `G92 E0` at every layer change, so that dialect lost
  every raise. Holding a `G92` means its origin reset can no longer move both
  streams at once: `observe_origin` runs when the line is read and
  `advance_origin` when it is written. The tail reuses `buffer`/`replay`, so
  ordering is preserved exactly — on a 70-layer file, 0 of 6638 non-move lines
  changed position. `TAIL` (64) caps it; the longest real tail before a wall
  measured 33 lines, on a Bambu profile with a timelapse macro. Cost: `brick`
  went 27.6 ms to 28.6 ms on the synthetic bench, 339 to 328 MB/s.
- **A rewritten line keeps its Marlin checksum true.** The serial dialect ends
  a line with `*nn`, the XOR of every byte in front of the `*`, and a firmware
  reading it stops parsing there. So a `Z` word appended behind the `*` is
  never seen and the raise silently does not happen, while an `E` rescaled in
  front of it leaves the stated sum stale and the whole line is rejected.
  `gcode::rewrite` puts new words **before** the `*` and recomputes the sum
  over the bytes it actually wrote.
- **A line read in relative or inch mode is written back exactly as found.**
  `Modal::is_plain` is `G90` and `G21` together, and it is the only state in
  which a coordinate this tool writes says what it means. A `G91`/`G20` stretch
  is custom G-code — a colour change, an MMU swap, a timelapse, a layer-change
  script — and never a perimeter, so nothing is given up by leaving it alone.
  It is still **measured**, so nothing downstream is misplaced: `Modal::apply`
  scales by 25.4 under `G20`, accumulates under `G91`, and takes a `G92` as a
  place however the mode reads a move. Feed it `Line::parse`, not `Line::scan`
  — `scan` drops `X` and `Y`, and a tracker that never sees them loses the
  position.
- **Do not measure the Z feedrate over the whole file.** `Scan` takes the
  slowest Z-only move, which before the `open_layer` gate was the start
  G-code's bed-clearance move on **every one of 28 real slices** — `G1 Z5 F300`
  against an in-print F600.
- **`--reorder-loops` is gone, and it bought nothing.** It existed to pay for a
  stop once a layer instead of once a loop; with the height riding a travel it
  removed zero stops and still added 19.1 m of travel. Do not bring it back
  without measuring stops on both sides.
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

A user reported stringing and blamed the wall flow multiplier for desyncing
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
  A file whose layers vary is therefore measured and nothing may override that;
  a `--layer-height` flag existed for it and was removed, since the case it
  mattered in — a slicer post-processing hook — is exactly where no one is
  there to pass it. There is **no `adaptive_layer_height` key** to grep for.
  The Z steps are the only signal.
- **A container states its own height and the G-code it carries does not.**
  `bgcode` metadata gives a nominal that the decoded text has no way to state,
  so the two paths only agree if the same figure reaches both. Measured on
  `mini_cube_ps2.8.1.bgcode`: the container says 0.2 and the Z-step histogram
  of its own decoded G-code measures 0.218, which raised by 0.109 instead of
  0.100. `tests/binary_gcode.rs` pins the two paths together by exporting
  `SLIC3R_LAYER_HEIGHT`, which is what a slicer does.
- **The histogram is the LAST answer, not the first.** A print retracts far
  more often than it changes layer, so where a Z-hop differs from the layer
  height the hop wins the vote outright — which is the whole of the 0.218
  above. `Scan::measured_height` differences each layer's own *floor* instead,
  and a floor is a per-layer minimum that a hop cannot reach; the histogram
  stands in only where there are no floors to difference at all, which is a
  file that lays no bead — a file with no layer markers has floors of its own,
  from `Markerless`. Do NOT put the histogram back in front: pinned by
  `scan::a_z_hop_is_not_mistaken_for_the_layer_height`. And whichever answer
  wins, it goes through `is_a_height` — without that a bed-clearance `G1 Z50`
  became the layer height and came back out as a commanded raise
  (`scan::a_measured_step_no_bead_could_be_is_refused`).
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
- **A file with no layer-change marker at all is laid out, not given up on.**
  `scan::Markerless` is that layout and **both passes use it**: a layer opens
  on the first bead laid off the plane the last one sat on. A boundary the
  survey and the rewrite disagree on is worse than no boundary at all, because
  every per-layer set is then consulted for the wrong layer, so there is one
  rule and both call it. It is the Z **move** that cannot be trusted — a hop
  lifts and comes back down before the next bead, so counting Z moves counted
  every hop as a layer and walked the rewrite's layer number away from the
  survey's for the rest of the file. The plane is `Markerless::plane()`,
  measured at the beads rather than accumulated, because the Z that reaches a
  layer is commanded while the layer before it is still open; that is also why
  `Pass::flush_before_a_layer` writes the loops still buffered at the layer
  that is **ending**, at its plane and metered for its height. Such a file
  therefore has floors to difference like any other, so `layer_heights`,
  `uncovered`, `unsupported`, `object_starts` and `object_tops` are all filled
  in for it and the nominal is no longer a fallback for the whole file. The old
  note here said the opposite — "`layer_heights` stays empty", "left alone on
  purpose" — and it was wrong: what reads a priming lift as a layer is the Z
  move, not the bead. A file that turns out to state its layers after all drops
  the lot (`Scan::forget_the_markerless_layout`); only a start G-code's purge
  line can reach that, so there is never more than one layer to drop. `zaa`
  still needs real markers and warns when it does not have them.

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
cargo run -- --bricks --verbose --output /tmp/processed.gcode <real.gcode>
python3 .github/skills/bricklayers/scripts/audit.py all /tmp/processed.gcode
```

**Pass the original file too whenever `zaa` also ran over it** — its output
deliberately no longer sits on one height per layer, so a plane read back out of
it is not the plane the slicer chose, and `invariant` will report every visible
wall bead as raised:

```sh
cargo run -- --bricks --zaa --verbose --output /tmp/both.gcode <real.gcode>
python3 .github/skills/bricklayers/scripts/audit.py invariant /tmp/both.gcode <real.gcode>
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
  "G1 Z0.300 F600 ; corbel brick raised" ";TYPE:External perimeter" \
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
| `src/brick/tests.rs` | its unit tests, in a sibling file so they do not bury it |
| `src/scan.rs` | `Survey` — one pre-pass for layer count, heights, feedrate, arc count, transform stamps; and `Markerless`, the layer layout for a file that states none |
| `src/gcode/feature.rs` | region marker classification for every slicer dialect |
| `src/slicer.rs` | `SLIC3R_*` settings the slicer exports before running the script |
| `src/gcode.rs` | byte-scanner line parser, `Code`, `Extruder`, `Modal`, checksum-preserving `rewrite` |
| `src/geometry.rs` | plane geometry both transforms share, re-exported flat |
| `src/geometry/footprint.rs` | where a layer's material sits, as grid cells; `Grid` picks how fine they are, and `Trace` says whether a walk could be finished |
| `src/geometry/inset.rs` | moving a closed loop sideways, toward the material behind it |
| `src/lib.rs` | `Source` (refuses what does not read as G-code) and `Sink` (exclusive temp file, rename, line endings put back, what a replacement cannot carry over) |
| `src/zaa.rs`, `src/zaa/` | the other transform — see [../zaa/SKILL.md](../zaa/SKILL.md) |

## Upstream

This is an independent Rust reimplementation of
[TengerTechnologies/Bricklayers](https://github.com/TengerTechnologies/Bricklayers).
The triage of that project's issues and pull requests — what is a real defect,
what was refuted by measurement, and what is still unverified — is in
[references/upstream-tracker.md](./references/upstream-tracker.md).
