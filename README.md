# corbel

**A post-processor for G-code, written in Rust. Supports BrickLayers and ZAA.**

Two independent transforms in one binary, each doing something to a print your slicer will not:

| | transform | what it does |
|---|---|---|
| 🧱 | **[BrickLayers](#-bricklayers)** — `--bricks` | makes layers **interlock** instead of stacking as independent flat sheets, which is exactly where FDM prints crack |
| 🪄 | **[Z anti-aliasing](#-z-anti-aliasing)** — `--zaa` | follows the model's surface **inside** a layer, so a shallow top comes out as a ramp instead of a staircase |

Run either, or both together in one pass. **You have to name at least one** — a run that names neither is refused, because this is handed your only copy of a file by a slicer that swallows everything it prints. Beyond the switch, neither transform needs anything filled in: the layer height, the line width and the flow are all read from your file.

```
corbel --bricks --zaa
```

---

## 📖 Contents

- [🚀 Install](#-install)
- [🖨️ Use](#️-use)
- [🧱 BrickLayers](#-bricklayers)
  - [📐 How much flow it adds](#-how-much-flow-it-adds)
- [🪄 Z anti-aliasing](#-z-anti-aliasing)
- [✨ Why this one](#-why-this-one)
- [🙏 Credits](#-credits)

---

## 🚀 Install

**One-liner.** Downloads the latest release into `~/corbel` (`%USERPROFILE%\corbel` on Windows), checks the published SHA-256 sums, and prints the line to paste into your slicer. Run it again any time to update in place.

```sh
# Linux / macOS
curl -fsSL https://raw.githubusercontent.com/erkexzcx/corbel/main/deploy.sh | bash

# Windows (PowerShell)
irm https://raw.githubusercontent.com/erkexzcx/corbel/main/deploy.ps1 | iex
```

> 🛡️ **Windows: "An Application Control policy has blocked this file".** These builds are unsigned, and [Smart App Control](https://support.microsoft.com/en-us/topic/what-is-smart-app-control-285ea03d-fa88-4d56-882e-6698afdb7003) blocks anything unsigned. Turn it off in **Windows Security → App & browser control → Smart App Control settings**.

**By hand.** Take your platform's file from the [latest release](https://github.com/erkexzcx/corbel/releases/latest) — Linux, macOS and Windows, x86-64 and arm64 — rename it to `corbel`, put it somewhere permanent and `chmod +x` it. If macOS refuses to run it, approve it in System Settings → Privacy & Security.

**From source.** Needs only [Rust](https://rustup.rs):

```sh
git clone https://github.com/erkexzcx/corbel.git
cd corbel && cargo build --release
```

---

## 🖨️ Use

One line goes into your slicer's **post-processing scripts** field: the binary's absolute path, followed by the transforms you want. The slicer appends the G-code path itself. Paste one of these, without the comment after it:

```
/home/you/corbel/corbel --bricks --zaa      # both
/home/you/corbel/corbel --bricks            # interlock the walls only
/home/you/corbel/corbel --zaa               # ramp the shallow tops only
```

On Windows use the full path, quoted if any folder name contains a space:

```
"C:\Users\you\corbel\corbel.exe" --bricks --zaa
```

In OrcaSlicer and Bambu Studio the field is under **Process → Others → Post-processing Scripts**, with the settings mode set to Advanced or Expert. Plain `.gcode` and `.bgcode` are both accepted, and the output keeps whichever came in.

> 🙈 **The preview will not show the change — and that is normal.** Post-processing runs after the slicer has already drawn its preview, so the toolpaths on screen are the *unmodified* ones. Every post-processing script in every slicer behaves this way; the exported file *is* processed. To see the result, export the G-code and open that file back in the slicer.

You can skip the slicer and run it yourself. `-o` writes a new file and leaves the input untouched; drop it and the file is rewritten in place. `-v` says whether anything happened:

```sh
corbel --bricks --zaa -v -o modified.gcode original.gcode
# corbel: 240 layers, 1365 perimeter loops, 533 raised by 0.100 mm
# corbel: 13 more were left flat where the wall ends and something is printed over it
# corbel: 5062.0 mm filament, 16.7% of it in raised loops; a flow of 1.025 adds 1.07% to the part
# corbel: 1168 surface moves on 86 layers followed from -0.080 to +0.100 mm of their plane, written as 3041 moves
# corbel: 69.9 mm filament in those surfaces, re-metered by -1.83% for the gaps they really cross
```

Zero raised loops means the file gave bricking nothing to work with — usually a single wall, or region markers spelled in a way that is not recognised. `no surface shallow enough to smooth` means the part has no shallow top, which a plain box genuinely does not.

### Options

Everything else is read from your file, so the whole command line is a path, a transform and where the result goes.

```
corbel [OPTIONS] <--bricks|--zaa> <GCODE>
```

**Shared**

| option | |
|---|---|
| `-o, --output <PATH>` | write here instead of overwriting the input |
| `-v, --verbose` | print a summary of what changed |
| `--force` | run anyway on a file this tool has already processed, which would stack a second pass on the first, or on one that does not read as G-code |
| `-h, --help` | the same list, grouped the same way |
| `-V, --version` | which release this is |

**🧱 Bricklayering**

| option | |
|---|---|
| `--bricks` | turn the transform on |
| `--extra-flow <PERCENT>` | extra flow for the walls, `0` to `50` (default `5`) — see [how much flow it adds](#-how-much-flow-it-adds) |

**🪄 Z anti-aliasing**

| option | |
|---|---|
| `--zaa` | turn the transform on |

It has no dials. How wide a stair tread is still worth following is a *slope*, so it comes out of your layer height on every layer — a 0.08 mm layer and a 0.28 mm one get the same shallowest angle rather than the same number of millimetres. How finely a surface is sampled comes from the grid the surface is measured on, and an arc is sampled for its own radius. Neither had an answer you could give better than the file could.

A dial belonging to a transform you did not name is accepted and ignored, so a leftover word in a slicer field never fails a print.

> 🔐 **Rewriting a file in place gives it a new identity, and some of the old one cannot be carried over.** The result is written beside the target and renamed over it, which is what makes a crash leave your original intact — but a rename publishes a new inode, so the owner, any POSIX ACL, any SELinux or AppArmor label and every other name hard-linked to the file stay behind with the old one. The permission bits are copied; the rest cannot be, because the standard library has no `chown` and no way to read an extended attribute, and neither is worth a dependency in a tool that is one binary with nothing to install. What can be detected is said out loud instead: run it under `sudo` on a file that is yours and it warns before it writes, naming both owners, and it warns when the file has other names linked to it, which keep the G-code as it was sliced. Run corbel as the owner of the file and none of this arises.

---

## 🧱 BrickLayers

`--bricks`

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="img/interlock-dark.png">
  <img alt="A wall's cross-section in three steps: as sliced, every gap between beads lines up into a channel through the wall; bricked, the same gaps are staggered but open up, because half of each is out of the nozzle's reach; bricked with extra flow, the flow fills those gaps and keys into them, ending tighter than as sliced with no channel left." src="img/interlock-light.png">
</picture>

*Seen end-on: columns are perimeter loops, rows are layers, and the red line is the join between two layers — the plane an FDM part splits along.*

**The middle step is the tool doing harm. The third is why it is worth it.**

1. **As sliced.** Two beads laid side by side leave a small gap where their rounded edges meet. Every layer breaks at the same height, so those gaps line up into a channel running straight through the wall — and that channel is where the part splits.
2. **Bricked.** Every other loop rises half a layer, so the gaps no longer line up. But they also get *bigger*: on a flat plane the nozzle's underside presses each gap shut as it passes, and over a staggered seam half of each one is out of its reach. Less bead touches bead than before. On its own, this step makes the joint weaker — which is exactly why it is not the whole story.
3. **Bricked + extra flow.** The material the walls gain fills those gaps and keys into them, so beads seat into each other instead of merely stacking. The wall ends with **more** contact than it had as sliced, and no aligned channel left to split along.

The gaps are the one thing drawn far larger than life — on a 0.2 mm layer they are a few microns across, and at true scale you would see nothing at all. Everything else comes out of the same code the binary uses, including the two details worth knowing: the layer on the build plate is left exactly as sliced, because nothing presses a bead there, and a raised column climbs to its half layer over two layers rather than stepping up in one.

**What it touches.** Every wall, and only walls — infill, bridges, gap fill and the surfaces come out exactly as sliced, so it is never a global flow bump. Two walls are enough; three or more interlocks twice as much.

**Two things happen inside the walls that are worth knowing.**

- **The first layer on the bed is left exactly as sliced.** Nothing presses a bead there, so surplus spreads sideways instead of filling anything. On a Benchy it filled in the recessed nameplate, which is exactly one layer deep.
- **The outer wall gets the flow and is then moved inward by half the width it gains.** Its neighbour is raised like any other, so it has the same staggered joint to feed — but a bead widens about its own centre, so half the gain would push the surface outward. Moving it in by that much sends the gain into the joint behind it and leaves the commanded outer face exactly where the slicer drew it. What it gains is `flow - 1` of its *spacing*, not of its nominal width: the area scales at a fixed height, and the bead's round edges cost the same either way.
- **A `G2`/`G3` arc moves with it.** It keeps the centre it was drawn about and changes radius by the offset — inward for an anticlockwise loop, outward for a clockwise one, which is into the material either way — and the `I`/`J` words that name that centre from the arc's start point are restated, because that start point moved. Measured against the input on two real arc-fitted slices: every moved arc's radius moved the right way, and the gap between the radius an arc is commanded at and the radius its endpoint lands at stayed inside what three-decimal coordinates already put there (0.45 µm at the median, against the input's 0.39 µm). A loop whose arcs cannot be moved without distorting the circle they were drawn on is left exactly as sliced; on a Benchy that is 3 beads of 21396, on a solid cylinder 5 of 684.

**What a printed part measures is a separate question.** The compensation above is exact on the toolpath — the visible wall's commanded outer face lands on the coordinate your slicer chose, down to the micron the three-decimal grid allows. Plastic is looser than a coordinate. Every wall behind the visible one gains material too and is *not* moved, because its gain is meant to fill the staggered joint rather than to be taken back; and a raised bead spends half a layer out of reach of the nozzle's flat underside, so what would normally be ironed flat is free to spread sideways. Both push the same way, and a bricked part can come out slightly over nominal in XY.

If yours does, there are two dials, in this order. `--extra-flow 0` meters every bead exactly as your slicer sliced it and moves no wall at all, leaving only the raise — that is bricking with nothing added, and it is the setting that isolates the raise from the flow. Beyond that, your slicer's XY size compensation trims a measured offset the way it would for any other change in flow.

---

### 📐 How much flow it adds

**`--extra-flow` is the extra a wall takes when your layer is as thick as your nozzle.** Print thinner than that — everyone does — and you get proportionally less:

> **extra flow ≈ `--extra-flow` × (layer height ÷ nozzle diameter)**

So the default of `5` gives **+2.5%** on a 0.2 mm layer through a 0.4 mm nozzle, because that layer is half the nozzle. Nothing to set; both numbers are read from your file.

**Why those two numbers.** A bead of plastic is not a rectangle in cross-section. It is a rectangle with a half-round bulge on each side. Lay two side by side and a small corner is left empty where the bulges meet; slicers close it by pulling the beads together until the overlap in the middle pays for the corners, and normally the nozzle's flat underside squashes what is left shut as it passes over.

Bricklayering lifts every other wall by half a layer, so that corner is now **half a layer below the nozzle** and out of its reach. The extra flow fills it instead. How big the corner is depends on exactly two things: the **layer height** (taller layer, taller corner) and the **line width** (wider bead, smaller share of it sitting in a corner). Both are stated in your G-code, so both are read, on every layer.

**What the default comes out at.**

| nozzle | line width | layer | layer ÷ nozzle | extra flow |
|---|---|---|---|---|
| 0.2 | 0.22 | 0.06 | 30% | +1.47% |
| 0.2 | 0.22 | 0.10 | 50% | +2.56% |
| 0.4 | 0.45 | 0.08 | 20% | +0.94% |
| 0.4 | 0.45 | 0.12 | 30% | +1.44% |
| 0.4 | 0.45 | 0.16 | 40% | +1.96% |
| **0.4** | **0.45** | **0.20** | **50%** | **+2.50%** |
| 0.4 | 0.45 | 0.24 | 60% | +3.06% |
| 0.4 | 0.45 | 0.28 | 70% | +3.65% |
| 0.6 | 0.65 | 0.15 | 25% | +1.24% |
| 0.6 | 0.65 | 0.20 | 33% | +1.68% |
| 0.6 | 0.65 | 0.30 | 50% | +2.61% |
| 0.8 | 0.85 | 0.20 | 25% | +1.26% |
| 0.8 | 0.85 | 0.28 | 35% | +1.80% |
| 0.8 | 0.85 | 0.40 | 50% | +2.66% |

Read down the 50% rows — 2.56, 2.50, 2.61, 2.66. Nozzle size barely matters on its own; what matters is **how thick your layer is next to your nozzle**, which is why one percentage covers every nozzle. It is `≈` rather than `=` because the flow actually follows the **line width** your file states, not the nozzle, and stock profiles set the width at 1.06 to 1.13 times the nozzle. Across the whole table that keeps it within **±7%** of the simple form; set an unusually narrow or wide line width and it shifts further, correctly.

**If you want more or less of it.** Set `--extra-flow` to anything from 0 to 50:

| `--extra-flow` | 0.4 mm nozzle, 0.2 mm layer | 0.4 mm nozzle, 0.28 mm layer |
|---|---|---|
| `0` | none — metered as sliced | none — metered as sliced |
| `2.5` | +1.25% | +1.83% |
| `5` (default) | +2.50% | +3.65% |
| `10` | +5.00% | +7.31% |
| `50` | +25.0% | +36.5% |

The top of the range is for sweeping a test print rather than for printing with. `0` is a real setting — bricking with the raise and nothing else, every bead metered exactly as your slicer wrote it. There is a ceiling, but it is not a number anyone picked: a bead can be widened until its edge reaches the centre of the loop beside it, past which it is swallowing its neighbour rather than filling the corner between them, and that works out at **×1.89** on the profile above. Swept over every nozzle from 0.2 to 1.2 mm at every layer height from 10% to 80% of it, nothing comes within 15% of it even at `50` — it is there to stop a file stating a width no slicer would emit, not to cap the dial.

This is the one thing worth reaching for if a printed wall tells you the default is wrong — the rest is geometry and stays read from the file. Note that your slicer's own per-filament flow ratio is the better place to compensate for a *material* that fills corners badly, because that affects the infill and the surfaces too.

**What it costs on the whole part.** Much less than the number above, because walls are only part of a print. Measured on two real slices at the default:

| part | filament in raised loops | added to the whole part |
|---|---|---|
| Benchy, 2 walls | 16.7% | **+1.07%** |
| Cylinder, solid wall throughout | 45.5% | **+2.30%** |

Turning your slicer's global flow up instead would tax the infill and the surfaces too.

**What gets it, and what does not.** Every wall, the one you can see included — measured per region on a real Benchy at the default:

| region | flow |
|---|---|
| Outer wall | **×1.0219** |
| Inner wall | **×1.0230** |
| Overhang wall | **×1.0226** |
| Solid infill, sparse infill, gap fill, bridges, top and bottom surface, brim | ×1.0000 |

So it is never a global flow bump. Those three sit a little under the 1.025 the profile asks for because the first layer on the bed is metered as sliced and pulls the average down.

**Where the two numbers are read from.** Whichever of these states them, in this order: the `SLIC3R_*` configuration your slicer exports when it runs a post-processing script, the metadata blocks of a `.bgcode` container, and the settings block a slicer appends to plain `.gcode`. The keys are `perimeter_extrusion_width` (PrusaSlicer, SuperSlicer) and `inner_wall_line_width` (OrcaSlicer, Bambu Studio); a width given as a percentage is resolved against `nozzle_diameter`. Layer heights are measured from the commanded Z, one layer at a time, so an adaptive slice is metered against what it actually printed rather than against its profile's nominal.

Binary G-code keeps all of this *outside* its G-code stream, so a search of the decoded text finds none of it — the metadata blocks are read instead. Checked against PrusaSlicer's own test files, which state a 0.4 mm nozzle and a 0.45 mm wall and come out at `1.025`.

If a file states no width at all — Cura writes its settings in a form nothing else parses — the walls fall back to the reference profile's flow, and `-v` says so rather than letting a default pass for a measurement. The inward move falls back with it, to the same profile: a wall that gains material without being moved grows the part by half of what it gained, so the two halves of the change are never applied apart.

**Where the formula comes from.** `spacing = width − height × (1 − π/4)` is the slicer's own bead model, not an invention here — PrusaSlicer computes exactly that in [`Flow::rounded_rectangle_extrusion_spacing`](https://github.com/prusa3d/PrusaSlicer/blob/master/src/libslic3r/Flow.cpp), and meters each bead at `height × spacing`. Verified against three real OrcaSlicer slices (0.4 mm nozzle, 0.2 mm layers, 0.45 mm inner wall, `filament_flow_ratio 0.95`): neighbouring loops measured **0.4074 mm** apart against the formula's 0.4071, and each bead metered **0.0773–0.0774 mm²** against the formula's 0.0774. A model that spaced beads at the nominal width would predict 0.0855 — 10.5% out — so any "void" figure derived from nominal width describes a slicer that does not exist.

The default — 5%, which is 2.5% on the commonest profile of all — is a **chosen constant**, not a measured one. It is small on purpose: it is paid on every wall of the part, and it also sets how far the visible wall is drawn in, which nobody wants measured in anything but microns. Only how it *scales* with your geometry is derived.

Published micro-CT work supports the direction, though none of it was used to fit these numbers:

- Faizaan, M. *et al.* **“A study on the overall variance and void architecture on MEX-PLA tensile properties through printing parameter optimisation.”** *Scientific Reports* **15**, 3103 (2025). [doi:10.1038/s41598-025-87348-2](https://doi.org/10.1038/s41598-025-87348-2) — PLA printed at 100% infill with a **concentric** pattern, i.e. the many-walls case. Void area fraction ran **0.117% to 4.99%** of the printed cross-section; the largest voids came from a 0.6 mm nozzle at 0.3 mm layers and the smallest from a 0.8 mm nozzle at 0.15 mm layers. Voids were **axially connected in every reconstruction** — the aligned channel that staggering the seams exists to break up.
- Guessasma, S., Belhabib, S. & Altin, A. **“On the Tensile Behaviour of Bio-Sourced 3D-Printed Structures from a Microstructural Perspective.”** *Polymers* **12**, 1060 (2020). [doi:10.3390/polym12051060](https://doi.org/10.3390/polym12051060) — overall porosity of 3D-printed PLA measured at **5.73%** by micro-CT, and the paper notes the same quantity comes out near **11%** when taken from weight and volume instead.

Both report the void a print has *before* anything is staggered, at a resolution far coarser than the corner this tool feeds. They are evidence that the corner is real and that it grows with layer height against nozzle size, which is exactly what the formula above scales with.

---

## 🪄 Z anti-aliasing

`--zaa`

**A layer is flat and your model usually is not.** Wherever a surface is shallower than about 45° the print leaves a staircase: each tread is one layer's worth of surface laid at one height, each riser is the full layer height, and the treads are what catch the light. This follows the surface across each tread instead — varying the height of the extrusion *within* the layer by up to half a layer either way, and metering each stretch for the gap it actually crosses.

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="img/contour-dark.png">
  <img alt="A shallow slope's cross-section in two steps: as sliced, every bead of a course is laid at one height and the model's surface cuts straight through them; followed, each bead sits at the height the surface really is and is metered for the gap under it, so the tops of the beads land on that line and consecutive treads join." src="img/contour-light.png">
</picture>

*Seen end-on, on a 7° slope: columns are beads, rows are layers, blue is the wall you can see, and the dashed line is where the model's surface really runs. The beads are the ones a real slice lays — about four to a tread at 0.2 mm layers — so what you get is a finer staircase rather than a perfectly smooth one, with a step a quarter the size of the one it replaced. The shallower the slope, the more beads share the tread and the smaller that step gets.*

**Consecutive treads join.** One ends half a layer **above** its plane exactly where the next begins half a layer **below** its own, so the full-height riser between them is gone — what is left is the much smaller step from one bead to the next within a tread.

**It does not need your model.** Every other implementation of this idea raycasts the original mesh — [GCodeZAA](https://github.com/Theaninova/GCodeZAA) asks you to export an STL per object and type in its position; [BambuStudio-ZAA](https://github.com/adob/BambuStudio-ZAA) does it inside the slicer, where the mesh is there anyway. A post-processor is handed G-code and nothing else, so this recovers the surface from the file. It can, because **a slicer takes its cross-section through the middle of a layer**: a layer's outline is where your model's surface passes half a layer *below* the plane, and the next layer's outline is where it passes half a layer *above*. Between the two the surface climbs from one to the other, and a straight climb across that strip reproduces a flat slope exactly — which is the case that stair-steps in the first place.

**What it touches.** The top surface, the ironing over it, and the walls. The walls matter more than they sound: a slope steeper than about 13° leaves a tread narrower than the wall stack standing on it, so your slicer emits no top-surface region at all and the staircase is made entirely of wall. The wall you can see is always followed. The walls behind it are followed too when this runs on its own, and left alone when bricklayering is running in the same pass — that transform is already moving them, and lowering a wall onto a bead it has raised would close a gap your slicer metered open. Infill, bridges and anything with a layer printed over it come out exactly as sliced.

**What it costs.** Each stretch is metered for its own gap, so a stretch sitting low takes less material and one sitting high takes more; over a whole tread the two cancel. Measured with `--zaa` alone, so the figures are this transform's and nothing else's:

| part | followed | how far off their plane |
|---|---|---|
| 60 mm spherical cap | 2678 moves, 17 of 90 layers | −0.074 to +0.100 mm |
| 180 mm spherical cap | 15824 moves, 52 of 240 layers | −0.082 to +0.100 mm |
| 60 mm cone, 1.9° | 1712 moves, 5 of 20 layers | −0.091 to +0.100 mm |
| Benchy, 2 walls | 1608 moves, 86 of 240 layers | −0.081 to +0.100 mm |
| Cube, and a flat plate with a boss on it | nothing — every face is vertical or flat | — |

A followed stretch is written as a short run of moves rather than one, so the exported file grows by a few per cent on a part with shallow tops and not at all on a part without them. Nothing is added anywhere else in the file.

**A minority of layers is the right answer, not a shortfall.** A staircase only shows where a layer leaves a tread wider than the bead standing on it; steeper than that the wall covers its own step and there is nothing left to smooth. On the 60 mm cap, 19 of its 90 layers leave such a tread and 17 of them are followed. Weighted over those layers alone, the surface comes out **0.32 of half a layer** away from where the slicer put it, across 56% of their length — that is the share of a removable step that is actually removed.

**A Benchy is a poor way to judge this, and that is the part's fault rather than the transform's.** Its hull flares outward, so nearly every face of it is an overhang, a vertical wall or a flat deck: 18 of its 241 layers leave a tread at all, against 18 of the cap's 91, and the surfaces this reshapes hold 111 mm of its 5064 mm of filament. Over its tread layers it removes **0.03 of half a layer across 8.8%** of the wall and top-surface path there, where the cap gets 0.32 across 56%. Print a shallow dome, a low cone or a wide chamfer if you want to see the difference.

The file grows because a curve needs a move per bend. A **straight** climb does not: your printer interpolates height along a move already, so a ramp is written as one move however far it runs.

**The wall you can see is only ever lowered, never raised.** That is the same rule bricklayering follows, and for the same reason: a bead of the outer wall standing proud is out of reach of the nozzle's flat underside, so what would be ironed level is free to bulge — on the face of the part. Where the surface genuinely rises at the wall, the wall stays put and the top surface beside it does the climbing.
**How shallow it goes is worked out from your layer height, and there is nothing to set.** The widest tread it will follow is the one a **1° slope** leaves, which is your layer height over the tangent of that — 11.5 mm at 0.2 mm layers, 4.6 mm at 0.08 mm ones. Stating it as an angle rather than as a width is what makes it mean the same thing on every profile, and it moves with an adaptive slice layer by layer. Everything down to that slope is followed at full amplitude; the fade runs over a further quarter *past* it, on surfaces shallower still, so the widest tread it follows does not end in a step of its own.

Nothing else has to be shallow *enough*: what separates a slope from a flat top or from a ledge is the layer **below**, not a width. A flat plate, and a flat plate with a boss standing on it, both come back byte-identical however wide their treads measure.

**What it will not do.**

- **A flat top is left alone.** Nothing is printed above it, so there is no far edge to climb to and no way to tell where in the layer the surface really is. Lowering it and metering it for half a gap would starve a surface that was correct as sliced.
- **A ledge with a wall standing on it is left alone.** Its tread looks like a slope's and is not one — the surface stops dead at the ledge's edge. The layer below tells them apart: under a real slope it reaches a tread further out, under a vertical face it stops in the same place.
- **A straight bore is left alone.** A hole that opens upward — a countersink, a chamfered mouth, a funnel — *is* followed, on the same terms as an outer slope: the layer above has to open it wider all the way round, and by at least two cells of the grid. A bore that goes straight down leaves no tread to follow, and a flat plate with a hole in it is unaffected either way.
- **It cannot see what your slicer did not write down.** A surface that curves sharply inside one tread is approximated by a straight climb, which errs toward the plane rather than away from it.

---

## ✨ Why this one

Common to both transforms:

- 🔀 **Two transforms, one pass** — run together they compose in a single read of the file, and neither disturbs the other: they own different regions of the print.
- 🎛️ **Nothing to fill in** — the line width and the nozzle come from your file and your slicer, the layer height is measured off the print itself layer by layer, and how shallow a surface has to be to get followed is derived from that height rather than typed in.
- 🔬 **Nothing to check first** — PrusaSlicer, SuperSlicer, OrcaSlicer, Bambu Studio and Cura all go through the same code path, with no slicer to pick and no dialect to declare.
- 📦 **One binary** — Linux, macOS and Windows, x86-64 and arm64, and nothing else to install.
- 🪶 **Any file size** — streamed rather than loaded, so a 300 MB slice costs the same memory as a small one: what either transform holds is bounded by one layer, never by the file.
- 🈳 **Any character set** — the file is read as bytes, so an object or filament name in any encoding passes through untouched rather than stopping the run.
- 🔎 **You can see what it did** — every change is stamped into the exported G-code, so `grep corbel` on the file tells you it ran and where, even though a slicer swallows everything a script prints.
- 🛡️ **Your file cannot be destroyed** — it is written aside and moved into place, a file that does not read as G-code is refused before a byte is written, a second run over the same file is refused rather than stacking a second shift on the first, and a run that names no transform does nothing at all.

Against [GeekDetour/BrickLayers](https://github.com/GeekDetour/BrickLayers) and [TengerTechnologies/Bricklayers](https://github.com/TengerTechnologies/Bricklayers), both of which are Python scripts that ask you to change slicer settings before they will work:

- 🐍 **No Python** — nothing to install and keep working, and no interpreter path in front of the script path in your slicer's field.
- 🎛️ **No numbers to keep in sync** — no `-layerHeight` to match your profile and no extrusion multiplier to guess, because both are read from the file on every layer.
- 🔧 **No slicer settings to change first** — arc fitting stays on, and your wall order is read rather than dictated.
- 🗜️ **Binary G-code just works** — `.bgcode` is read and written natively, thumbnails and slicer config copied byte for byte, where the others need it turned off.
- 🧵 **No stringing from the raise** — a height change rides a travel the printer was already making, instead of stopping the toolhead over a seam with a primed nozzle.
- 🧩 **Two walls are enough** — a region with one internal loop is bricked against the wall you can see rather than skipped, so thin ribs and lone loops are interlocked too.
- 🎨 **The visible wall is in on it** — it takes the same flow as every other wall, and is drawn back in by half of what it gains so its outer face stays exactly where your slicer drew it.
- 🚫 **Nothing else is touched** — bricklayering re-meters walls and nothing else, so infill, bridges, gap fill and the surfaces come out exactly as sliced; it is never a global flow bump.
- 🛟 **The awkward files still come out right** — adaptive layer height, absolute extrusion and objects printed one at a time are each metered for what they are.
- ⚡ **Fast** — an 18 MB, 925-layer slice of a 250 mm duct goes through `--bricks` in 1.3 s at 14.0 MiB, and through both transforms in 15.7 s at 43.9 MiB; a Benchy takes 1.3 s at 20.8 MiB. Bricking's memory is flat whatever the file size; the surface transform spends a fixed budget of grid cells on whatever the part spans, so a small part gets a finer grid rather than a smaller one and a Benchy costs the same for the same work. A part that fills the largest bed there is gets a coarser grid, not a refusal.

Against [Theaninova/GCodeZAA](https://github.com/Theaninova/GCodeZAA), the post-processor that Z anti-aliasing started as:

- 🧊 **No STL to export** — it does not need the model. Where the surface sits is recovered from the outlines the slicer already wrote into the file, which is exact for a flat slope and errs toward the plane for anything else.
- 🎯 **No object name or position to type in** — nothing to keep in sync with your plate.
- 📏 **No reach or resolution to pick** — how shallow a tread has to be is a slope, so it comes off your layer height on every layer; and the grid the surface is measured on is chosen from the size of your part, spending a fixed budget of cells so a small part is measured finely rather than cheaply.
- 🌀 **Arcs work** — `G2`/`G3` are resampled rather than skipped, so a file sliced with arc fitting on is not quietly left alone. Each one is sampled for its own radius, to within a micron of the arc it replaces.
- 🖨️ **Any firmware** — no Klipper requirement, and no wall order to set first.

---

## 🙏 Credits

Neither idea started here. This is a Rust implementation of both, without their prerequisites.

**BrickLayers** — the idea and the research behind it belong to:

- [TengerTechnologies/Bricklayers](https://github.com/TengerTechnologies/Bricklayers) — the original post-processing script, and the video that started it.
- [GeekDetour/BrickLayers](https://github.com/GeekDetour/BrickLayers) — the fork that worked out most of what a real slice actually needs.

**Z anti-aliasing** — the idea and the two implementations this one learned from:

- [adob/BambuStudio-ZAA](https://github.com/adob/BambuStudio-ZAA) — the transform built into a slicer fork, where the mesh is on hand to raycast.
- [Theaninova/GCodeZAA](https://github.com/Theaninova/GCodeZAA) — the standalone post-processor, and the source of the seam rule that keeps the visible wall from ever standing proud.
- [Song et al., *Anti-aliasing for fused filament deposition*](https://arxiv.org/abs/1609.03032) — the paper both of those trace back to.

Licensed GPL-3.0-or-later.
