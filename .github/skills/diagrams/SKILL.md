---
name: diagrams
description: Generate the README's wall cross-section diagrams from the same bead model the binary uses, and verify they have not drifted from it. Use when a diagram in README.md needs regenerating or restyling, when a constant in src/brick.rs changes and the picture must follow, when adding a new figure about bricking geometry, and before trusting any illustration of layer height, bead width, seam stagger, extrusion flow or the visible wall's inward offset.
---

# Diagrams

The pictures in README.md are **generated from the binary's own arithmetic**, not drawn. A
figure that disagrees with the code is worse than no figure, because a reader will believe it.

Everything lives in [scripts/](./scripts):

| file | |
|---|---|
| `beads.py` | the bead model, function for function the twin of `src/brick.rs` |
| `render.py` | draws the panels and writes the PNGs |
| `pin.py` | proves `beads.py` still agrees with the Rust **and with the compiled binary** |

## Regenerating

```sh
python3 .github/skills/diagrams/scripts/pin.py       # must pass first
python3 .github/skills/diagrams/scripts/render.py --output-dir .
```

`render.py` writes `interlock-light.png` and `interlock-dark.png` into the repo root, which is
where README.md's `<picture>` block looks for them. Both are committed: GitHub cannot run a
script, and a diagram that only exists on someone's laptop is a diagram that rots.

Needs `matplotlib`. Nothing else, and nothing is added to `Cargo.toml` — this is documentation
tooling, not a dependency of the binary.

## The rule

**Never hard-code a coordinate.** Every position, height and width in `render.py` comes back
from `beads.py`, which is the mirror of `brick.rs`. If a picture needs a number, add the
function that derives it; do not measure it off the last render.

`pin.py` enforces this from both ends:

1. It reads the constants straight out of `src/brick.rs` and compares them with `beads.py`.
   A retuned `DEFAULT_EXTRA_FLOW` or a renamed `REFERENCE_WIDTH` fails here.
2. It builds the release binary, runs it over a synthetic PrusaSlicer-shaped slice, and checks
   that the raise heights and the visible wall's offset in the output are the ones `beads.py`
   predicts. This is what catches a formula that is right in isolation and wrong in place.

It **always** rebuilds. A stale `target/release/bricklayers` is exactly the drift the script
exists to catch, and it would blame the Rust for it.

## What the picture has to get right

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

## Changing the figure

`render.py --help` exposes the geometry: `--layers`, `--loops`, `--height`, `--width`,
`--skin-width`, `--extra-flow`, `--capped`. Use them to sanity-check a change — an
adaptive-height slice, a two-loop wall, a column capped where something is printed over it —
before settling on what README.md ships. The shipped figure runs uncapped, so every column
reaches full height and the raised ones stand their half layer proud, which is what a wall
does where it simply carries on.

Two things to keep if you restyle it:

- **The red line is the join between two layers**, drawn across the whole wall. It is the plane
  an FDM part splits along, and its going flat in the first panel and stepped in the other two
  *is* the argument.
- **Both themes.** GitHub serves the dark PNG through `prefers-color-scheme`, and a white slab
  in a dark README looks broken.
