---
name: diagrams
description: Generate the README's cross-section diagrams from the same models the binary uses, and verify they have not drifted from it. Use when a diagram in README.md needs regenerating or restyling, when a constant in src/brick.rs or src/zaa/surface.rs changes and the picture must follow, when adding a new figure about bricking or surface-following geometry, and before trusting any illustration of layer height, bead width, seam stagger, extrusion flow, the visible wall's inward offset, or where a model's surface sits inside a layer.
---

# Diagrams

The pictures in README.md are **generated from the binary's own arithmetic**, not drawn. A
figure that disagrees with the code is worse than no figure, because a reader will believe it.

Two figures, one per transform:

| figure | transform | what it argues |
|---|---|---|
| `img/interlock-*.png` | BrickLayers | staggering the seams opens the corners up, and the flow is what pays for that and then some |
| `img/contour-*.png` | Z anti-aliasing | a course laid at one height misses the surface by half a layer; followed, every bead's top lands on it |

Everything lives in [scripts/](./scripts):

| file | |
|---|---|
| `beads.py` | the bead model, function for function the twin of `src/brick.rs` |
| `surface.py` | the surface model, the twin of `src/zaa/surface.rs` and `src/zaa.rs` |
| `render.py` | draws the interlock panels and writes its PNGs |
| `contour.py` | draws the surface panels and writes its PNGs |
| `pin.py` | proves both models still agree with the Rust **and with the compiled binary** |

## Regenerating

```sh
python3 .github/skills/diagrams/scripts/pin.py       # must pass first
python3 .github/skills/diagrams/scripts/render.py
python3 .github/skills/diagrams/scripts/contour.py
```

Both write into `img/` at the repo root, which is where README.md's `<picture>` blocks look.
All four PNGs are committed: GitHub cannot run a script, and a diagram that only exists on
someone's laptop is a diagram that rots. **A figure carries text, so a rename reaches it** —
the interlock pair said `what bricklayers does by default` for a while after the binary stopped
being called that, because nobody re-ran the script. It said `what corbel does by default` for
a while after that, which was wrong twice over: naming no transform is refused, so nothing is
done by default. Every panel that a switch produces now names that switch — `what --bricks
--extra-flow 0 does` on the middle interlock panel, `what --bricks does` on the fed one, `what
--zaa does` on the followed slope — in the same words, size and ink, from `render.SWITCH_SIZE`,
which `contour.py` imports so the two figures cannot drift apart. The label lives in the panel's
own tuple in `render.steps`, not in an index test, so adding a panel cannot mislabel one.

Needs `matplotlib`. Nothing else, and nothing is added to `Cargo.toml` — this is documentation
tooling, not a dependency of the binary.

## The rule

**Never hard-code a coordinate.** Every position, height and width in `render.py` and
`contour.py` comes back from `beads.py` or `surface.py`, which are the mirrors of the Rust. If a
picture needs a number, add the function that derives it; do not measure it off the last render.

`pin.py` enforces this from both ends:

1. It reads the constants straight out of `src/brick.rs`, `src/zaa/surface.rs` and `src/zaa.rs` and
   compares them with the Python. A retuned `DEFAULT_EXTRA_FLOW`, a renamed `REFERENCE_WIDTH` or
   a changed `FADE` fails here, as does deleting the line that caps the visible wall.
2. It builds the release binary and runs it over two synthetic slices — a PrusaSlicer-shaped
   cube for bricking and a wedge for the surface — and checks that the raise heights, the
   visible wall's offset and the followed heights in the output are the ones the models predict.
   This is what catches a formula that is right in isolation and wrong in place.

It **always** rebuilds. A stale `target/release/corbel` is exactly the drift the script
exists to catch, and it would blame the Rust for it.

## What the interlock picture has to get right

The figure is three panels, and the order is the argument — including the step where the tool
makes things *worse*:

1. **as sliced** — the gap every pair of beads leaves lines up into a channel through the wall.
2. **bricked** — the same gaps are staggered so nothing runs straight, but they also *open up*:
   the nozzle's underside presses a corner shut on a flat plane, and half of each corner is now
   half a layer below it and out of reach. On its own this panel is the worst of the three, and
   the figure has to say so or the third panel means nothing.
3. **bricked + flow** — the flow fills those corners and keys into them, ending tighter than
   panel 1 with no aligned channel left.

The caption must not claim they close: the flow feeds a corner, it does not abolish one, and a
panel with no gap in it would claim something the tool does not do. Everything else is
supporting detail.

These are the details a hand-drawn version gets wrong, every one of them observable in the
output of the real binary:

- **Beads tile; they do not overlap.** A bead is laid `spacing = W − h(1 − π/4)` from its
  neighbour, which is *less* than its width, so drawing two at full width merges them into a
  blob. `render.wall` draws what each one owns — midpoint to midpoint — so the courses read as
  brickwork. Measured at 0.4074 mm on a real OrcaSlicer slice against the formula's 0.4071.
- **A loop's own width still sets where it reaches.** The outermost face is the visible wall's
  real half-width out from its real centre, which is what keeps that face still while the flow
  widens the bead. Every other boundary — the ones between loops, and the innermost face — stays
  where the slicer put the centres, so the flow shows up in the joint, which is where it goes,
  and not as a wall growing sideways.
- **The visible wall's outer corners never close.** The face of the part is free air — no flow
  presses that edge into a corner — so it keeps its as-sliced profile at any flow. Only the
  joints behind it are fed.
- **The flow goes into the width; the span goes into the height.** A bead metered at
  `flow × span` is `span × h` tall and `flow × spacing + (span × h)(1 − π/4)` wide. So a
  climbing bead is taller and no wider, and a capped one is half height and no narrower.
- **The layer on the plate is never raised and never metered over.** Its beads are drawn at
  flow 1 — which is why the bottom course still has open corners in the third panel — and the
  visible wall on it is not moved either, because `move_walls` declines the whole layer.
- **A column climbs over `RAMP` layers.** Layer 1 stands at a quarter of the height, layer 2
  and up at half. Drawing the full offset from layer 1 shows a step the binary does not make.
- **The visible wall is loop 0 and is never raised.** Its neighbour is. Parity runs outward
  from the wall you can see, which is what `Pass::number_loops` computes.

## The one thing drawn out of scale

`GAP` in `render.py` sets how open a corner is drawn. At true scale that corner is microns
across beside a 0.2 mm layer, so a faithful figure would show nothing at all. It is the **only**
quantity in the picture that is not the binary's own arithmetic, README.md says so under the
figure, and it must stay that way — do not "exaggerate" a width, an offset or a raise to match
it.

Nothing else is written into the image. The panels carry a title and one caption each; every
explanation lives in README.md, where it reads at any width, is searchable, and does not have
to be re-rendered to fix a typo. Numbers in particular stay out: a flow multiplier or a micron
offset printed on the figure ages the moment a constant moves.

What happens to that corner is not a styling choice, though. Three functions carry it, and each
one is a physical claim the README already makes:

| | |
|---|---|
| `UNREACHED` | a corner beside a staggered seam is drawn **wider**, because half of it is below the nozzle |
| `gap_at` | the flow narrows it, from fully open at the slicer's flow down to `SHUT` at the most `--extra-flow` can ask for on that geometry |
| `key_at` | how far the fed material bends the boundary between two loops |
| `FLATTEN` | clips the wave so a crest is a short straight run, not a point |

The boundary is **one curve that both loops are cut from**, a clipped cosine of exactly one
layer's period, and its sign follows the parity. One column stands half a layer above the
other, so where one bead's middle pushes out, the joint between two of its neighbour's beads is
there to take it, and half a layer up the roles swap. The two bricks therefore *snap* together
— no overlap — which is what the flow does to a staggered seam. Do not go back to bowing each
bead independently: two neighbours then bulge into each other and only the z-order hides it.

**The crests must stay flat, and smoothly so.** A pointed crest puts the corner arcs at a
junction on a slope, which kinks the outline and closes the junction up. Three beads meet there
and a round-cornered bead cannot fill a Y, so that void is real and has to show. `FLATTEN` has
to be large enough that a crest is at least one corner radius long, and it *saturates* rather
than clipping — a clip puts a hard corner at every transition, and a bead of plastic has none.

**The two climbing layers mate too, and that is what `phase_of` is for.** The key troughs at
the raised column's joints and crests at its middles, and those are read off that column's
*real* beads rather than off a fixed one-layer period. It matters because the two climbing
beads are a quarter layer taller than the rest: against a fixed period they drift out of step
and the bricks tear. Both earlier attempts — a fixed period, and gating the key off during the
climb — were tried and rejected; the second turns the bottom of the wall into plain rectangles.

**Every corner arc is centred a radius in from the curve where its own side starts**, never
from the curve's value at the joint. Anchoring at the joint looks right on a crest and steps by
about ten microns on a slope — which is exactly where a climbing layer's joints land, and it
showed as a notch on the inner columns. Tapering the radius only masks it; centring the arc
where the side begins makes the step impossible.

Only the two faces at the ends of the stack take no wave. The outer one is free air, and the
inner one has infill against it rather than another loop.

The third panel therefore ends **tighter than the first**, which is the only honest way to draw
a tool whose middle step opens a void up.

## What the contour picture has to get right

Two panels, and the argument is the dashed line that runs through both: **where the model's
surface really is**. As sliced it cuts straight through each course; followed, every bead's top
lands on it. Nothing else in the figure has to be believed for that to be visible.

That line is one straight line for the whole slope, and that is not a simplification —
`surface.surface` derives it. A slicer takes its cross-section through the middle of a layer, so
a layer's outline is where the surface passes half a layer below that layer's plane.
Substituting that into `plane_of` and `share` cancels the layer index away. It is also *why*
consecutive treads join instead of stepping, so drawing the line per-tread would throw away the
one thing the figure is for.

These are the details a hand-drawn version gets wrong, every one of them derivable:

- **A tread is made of beads, and they do not blend.** At 0.2 mm layers on the shipped 7° slope
  a tread carries about four of them, so what the transform leaves is a **finer staircase**,
  with a step of `spacing / strip` — a quarter — of the riser it replaced. Drawing a smooth
  ramp would claim something the tool does not do. It also says the right thing about the dial:
  the shallower the slope, the more beads share the tread and the smaller the residual step.
- **Every bead's bottom is its layer's plane less one layer, flat.** What sits under a strip is
  covered by the layer above it, so it was laid flat, and *that* is what lets a stretch be
  metered for `height + rise` and fill exactly. If the picture ever shows a bead sitting on a
  sloped one, the metering it draws is wrong.
- **The rise is quantised.** `surface.quantise` puts it on one of `STEPS` steps of a layer,
  because `Field::rise` is a signed byte. It is a fifth of a micron, invisible, and it is in the
  model so that nothing else has to be approximate.
- **The visible wall is only ever lowered.** Its centreline already stands half a bead inside
  the layer's own face, so on any tread wider than that the surface there is *below* the plane
  and the cap never binds — which is exactly why the cap costs so little. `surface.WALL_CEILING`
  is what holds it, and `pin.py` fails if `zaa.rs` stops applying it.
- **The frame is cut mid-tread at both ends.** On a step it would cut a course where the lattice
  of beads and the edge of a strip disagree, leaving one bead of the layer below half exposed at
  the very edge and one of the top course with nothing drawn over it — both read as notches the
  transform had put there.
- **A course past the frame is drawn.** Without it the top tread's own covered bead has nothing
  over it. It costs one loop iteration and removes the last artefact.

The **flat top is not in the picture**, and that is deliberate: it is the case the transform
declines, so a figure whose top course was flat would be showing the tool doing nothing at the
very place the eye lands. The slope runs off the top-right corner instead.

## Changing the figures

`render.py --help` exposes the interlock geometry: `--layers`, `--loops`, `--height`, `--width`,
`--skin-width`, `--extra-flow`, `--capped`. Use them to sanity-check a change — an
adaptive-height slice, a two-loop wall, a column capped where something is printed over it —
before settling on what README.md ships. The shipped figure runs uncapped, so every column
reaches full height and the raised ones stand their half layer proud, which is what a wall
does where it simply carries on.

`contour.py --help` exposes the slope: `--treads`, `--strip`, `--height`, `--width`, `--skin-width`, `--reach`. `--strip` is the tread width, which is the layer height over the tangent of the slope, so it is the dial that sets how shallow the drawn surface is. **`--reach` is the script's only dial that the binary does not have** — there it is derived per layer, as the tread a `SHALLOWEST_SLOPE` surface leaves, and `surface.reach_for` is that derivation. It is exposed here because the fade is otherwise unreachable in a figure: the amplitude is full while the strip stays under three quarters of the reach, fades over the rest, and is gone once the two are equal — set `--strip 4 --reach 4` and every bead should come back flat. That is a good check that the figure is still reading `surface.py` rather than a remembered number.

Three things to keep if you restyle either:

- **The red line.** In the interlock figure it is the join between two layers — the plane an FDM
  part splits along — and its going flat in the first panel and stepped in the other two *is*
  the argument. In the contour figure it is the model's surface, and its being cut through in
  the first panel and ridden in the second is the same kind of argument.
- **One palette.** `contour.py` imports `Theme`, `LIGHT`, `DARK`, `GAP` and `brick` from
  `render.py`, so the two figures cannot drift apart in colour or in how a bead is drawn. Blue
  is the wall you can see and orange is a bead the tool moved, in both.
- **Both themes.** GitHub serves the dark PNG through `prefers-color-scheme`, and a white slab
  in a dark README looks broken.
