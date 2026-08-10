# 🧱 bricklayers

**A G-code post-processor that makes 3D printed layers interlock instead of stacking as
independent flat sheets — which is exactly where FDM prints crack.**

```
      without                    with `brick`
   ┌───┬───┬───┐               ┌───┬───┬───┐
   │   │   │   │               │   ├───┤   │
   ├───┼───┼───┤               ├───┤   ├───┤
   │   │   │   │               │   ├───┤   │
   ├───┼───┼───┤               ├───┤   ├───┤
   │   │   │   │               │   ├───┤   │
   └───┴───┴───┘               └───┴───┴───┘
    seams align                seams interlock
```

*Seen end-on: columns are perimeter loops, rows are layers. Every bead is the same height —
only the seams move.*

> 💡 **Credit where it's due** — the idea and the research behind it belong to **Roman Tenger**
> and [TengerTechnologies/Bricklayers](https://github.com/TengerTechnologies/Bricklayers). Go
> star that repo and watch [the video](https://www.youtube.com/@TengerTechnologies) explaining
> why any of this works. This is a from-scratch Rust implementation, not a port.
>
> 🤖 **This project is fully vibe coded.**

---

## ✨ Features

- 🧱 **Interlocks your layers** — every other internal perimeter loop rises half a layer, so
  seams stagger instead of stacking.
- 🧵 **No stringing** — a height change rides a travel the printer was already making.
- 🎨 **Visible surfaces untouched** — external perimeters, top layers and the bed layer never
  move.
- 🧩 **Two walls is enough** — thin ribs and lone loops are bricked too.
- 💧 **Only raised loops get extra flow** — a fraction of what a global flow bump costs.
- 🎛️ **Nothing to configure** — every setting it needs comes from your slicer.
- ⚠️ **Warns when a setting defeats it** — spiral vase, single-wall regions, odd layer heights.
- 🔬 **The awkward cases are handled** — adaptive layer height, walls that start or stop partway
  up, sequential objects, absolute extrusion, arc fitting.
- 🈳 **Any encoding** — object and filament names in any character set pass through untouched.
- 🔁 **Refuses to run twice** — no stacking a second shift on the first.
- 📊 **Tells you what it did** — `-v` reports loops raised, filament used, and what the
  multiplier cost.
- 🔌 **Slicer script or standalone** — wire it in, or run it over a file yourself.
- 📦 **Single binary** — no Python, no dependencies.
- 🖥️ **Linux, macOS, Windows** — x86-64 and arm64.
- 🗜️ **Plain and binary G-code** — `.gcode` and `.bgcode`, in and out.
- 🪶 **Any file size** — streamed, not loaded. A big slice takes under a second.
- 🛡️ **Your file cannot be destroyed** — written aside, then moved into place.

---

## 🚀 Install

**One-liner.** Downloads the latest release into `~/BrickLayers` (`%USERPROFILE%\BrickLayers`
on Windows), checks the published SHA-256 sums, and prints the line to paste into your slicer.
Run it again any time to update in place.

```sh
# Linux / macOS
curl -fsSL https://raw.githubusercontent.com/erkexzcx/Bricklayers-rust/main/deploy.sh | bash

# Windows (PowerShell)
irm https://raw.githubusercontent.com/erkexzcx/Bricklayers-rust/main/deploy.ps1 | iex
```

> 🛡️ **Windows: "An Application Control policy has blocked this file".** These builds are unsigned, and [Smart App Control](https://support.microsoft.com/en-us/topic/what-is-smart-app-control-285ea03d-fa88-4d56-882e-6698afdb7003) blocks anything unsigned. Turn it off in **Windows Security → App & browser control → Smart App Control settings**.

**By hand.** Take your platform's file from the
[latest release](https://github.com/erkexzcx/Bricklayers-rust/releases/latest) — Linux, macOS
and Windows, x86-64 and arm64 — rename it to `bricklayers`, put it somewhere permanent and
`chmod +x` it. If macOS refuses to run it, approve it in System Settings → Privacy & Security.

**From source.** Needs only [Rust](https://rustup.rs):

```sh
git clone https://github.com/erkexzcx/Bricklayers-rust.git
cd Bricklayers-rust && cargo build --release
```

---

## 🖨️ Use

One line goes into your slicer's **post-processing scripts** field: the binary's absolute path,
a space, then `brick`. The slicer appends the G-code path itself.

```
/home/you/BrickLayers/bricklayers brick --extrusion-multiplier 1.05
```

On Windows use the full path, quoted if any folder name contains a space:

```
"C:\Users\you\BrickLayers\bricklayers.exe" brick --extrusion-multiplier 1.05
```

In OrcaSlicer and Bambu Studio the field is under **Process → Others → Post-processing
Scripts**, with the settings mode set to Advanced or Expert. Plain `.gcode` and `.bgcode` are
both accepted, and the output keeps whichever came in.

> 🙈 **The preview will not show the change — and that is normal.** Post-processing runs after
> the slicer has already drawn its preview, so the toolpaths on screen are the *unmodified*
> ones. Every post-processing script in every slicer behaves this way; the exported file *is*
> processed. To see the result, export the G-code and open that file back in the slicer.

You can skip the slicer and run it yourself. `-o` writes a new file and leaves the input
untouched; drop it and the file is rewritten in place. `-v` says whether anything happened:

```sh
bricklayers brick --extrusion-multiplier 1.05 -v -o modified.gcode original.gcode
# bricklayers: 240 layers, 3277 internal loops, 1822 raised by 0.100 mm
# bricklayers: 7996.7 mm filament, 35.3% of it in raised loops; --extrusion-multiplier 1.05 adds 1.64% to the part
```

Zero raised loops means the file gave it nothing to work with — usually a single wall, or
region markers spelled in a way that is not recognised.

### 🧱 Options

```
bricklayers brick [OPTIONS] <GCODE>
```

| option | default | |
|---|---|---|
| `--extrusion-multiplier <FACTOR>` | `1.0` | extra flow for raised loops, `1.0` to `1.3` — see below |
| `--wall-order <ORDER>` | `auto` | `auto`, `external-first` or `internal-first` — override the wall order read from the file |
| `--layer-height <MM>` | auto | override detection |
| `--reorder-loops` | off | group a layer's loops by height instead of alternating — kept for curiosity, it no longer saves anything |
| `-o, --output <PATH>` | | write here instead of overwriting the input |
| `-v, --verbose` | | print a summary of what changed |
| `--force` | | run even on a file already processed, which would stack a second shift on the first |

**`--extrusion-multiplier` is not a global flow bump.** It rescales *only the loops this tool
raises*, so `1.05` here adds under 2% to a part's mass where your slicer's flow ratio set to
`1.05` adds 5%. Volume alone says `1.0`, which is the default; above that compensates for a
raised bead being laid against a *step* rather than a flat plane, and that is a constant of
your printer and filament rather than something derivable from G-code. `1.05` is a starting
point, not a recommendation.

**`--wall-order` is not a preference.** It states which order your slicer already prints a
region's walls in, because that decides which end of a wall the numbering starts from. Every
mainstream slicer says so in the file, so `auto` is nearly always right — and setting it wrong
roughly doubles how often the stagger inverts.

---

## 📄 Licence

GPL-3.0-or-later, matching the project that inspired it.
