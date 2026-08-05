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

*Seen end-on: columns are perimeter loops, rows are layers. Every bead is the same
height — only the seams move. The half-height cells are where the raised column meets
the bed and the top layer, the two places a brick wall needs a half brick too.*

> 💡 **Credit where it's due** — the idea and all the research behind it belong to
> **Roman Tenger** and
> [TengerTechnologies/Bricklayers](https://github.com/TengerTechnologies/Bricklayers).
> Go star that repo and watch [the video](https://www.youtube.com/@TengerTechnologies)
> explaining why any of this works. This is a from-scratch Rust implementation of the same
> ideas, not a port, and it behaves differently in places.
>
> 🤖 **This project is fully vibe coded.**

---

## 📑 Contents

- [✨ Features](#-features)
- [🧪 Slicer support](#-slicer-support)
- [⚠️ Limits, and what has not been tested](#%EF%B8%8F-limits-and-what-has-not-been-tested)
  - [Limits](#limits)
  - [Not tested at all](#not-tested-at-all)
- [🚀 Install](#-install)
  - [1. One-liner](#1-one-liner)
  - [2. Download by hand](#2-download-by-hand)
  - [3. Build from source](#3-build-from-source)
- [🖨️ Use](#%EF%B8%8F-use)
  - [🛠️ Running it by hand](#%EF%B8%8F-running-it-by-hand)
  - [`brick`](#brick)
  - [Shared](#shared)
- [📄 Licence](#-licence)

---

## ✨ Features

- 🧱 **`brick`** — raises every other internal perimeter loop by half a layer height, so loop
  seams stagger and layers key into each other.
-  **Single binary, zero runtime** — no Python, no dependencies, no install ceremony. Point
  your slicer at it and go.
- 🎛️ **Every slicer, one code path** — PrusaSlicer, SuperSlicer, OrcaSlicer, Bambu Studio and
  Cura region markers are all recognised by one classifier. No slicer detection, no flags.
  Tested mostly against OrcaSlicer, though — [see below](#-slicer-support).
- 🔎 **Reads your slicer's settings** — layer height, wall order and wall
  count come from the environment your slicer exports, so `--layer-height` is never needed,
  and settings that quietly defeat the transform (spiral vase, too few walls) get a warning.
  Bambu's renamed spellings are accepted too.
- 🧱 **The layer on the build plate is never touched** — a bead there is pressed against the
  plate rather than against a layer, so raising it presses nothing and the extra flow spreads
  sideways into whatever detail is beside it. A column climbs to its offset over the two
  layers above the bed instead, which costs the same filament and asks no bead to span more
  than a quarter of a layer beyond what your slicer metered it for.
- 📐 **Variable and adaptive layer height** — each layer is raised by half of *its own*
  height, measured from the file, and each raised bead is metered for the gap its own column
  actually left. Half of one nominal height would be wrong nearly everywhere: on an adaptive
  Benchy the layers run 0.081 to 0.119 mm while the profile still says 0.2.
- �️ **Printing objects one at a time is understood** — a file sliced to complete each
  object before starting the next holds several first and last layers, and each one is
  treated as such: every object's bed layer is left alone and its column climbs from there,
  and every object's top is left flat.
- �🗜️ **Binary G-code just works** — `.bgcode` is read and written natively, no conversion step.
  Thumbnails and slicer config are copied byte for byte and the file keeps the compression it
  arrived with.
- 🧮 **Absolute extrusion (`M82`) actually works** — rescaling in absolute mode shifts every
  later `E` value, so the whole downstream stream is rewritten to stay continuous. Cura
  defaults to this mode; the original scripts do not handle it.
- 🛟 **Your G-code file cannot be destroyed** — output goes to a temp file, flushed to the disk
  and moved into place only once complete, keeping the original's permissions. Or use
  `--output` and never touch the input at all.
- 🪶 **File size does not matter** — G-code is streamed, never loaded. Flat ~14 MB of memory
  whether the input is 2 MB or 2 GB, where loading a 307 MB slice whole needs 1.8 GB. A binary
  `.bgcode` container streams too, a block at a time: a 13 MB one holding a 30 MB slice goes
  through in 4 MB of memory.
- ⚡ **Fast** — a 31 MB, 1.2M-line slice goes through `brick` in about 150 ms.
- ⬇️ **The nozzle always comes back down** — a raised loop returns to the layer plane before
  anything else prints, so nothing downstream inherits the shift, and no move is ever commanded
  below the build plate.
- 🛡️ **Settings a printer cannot act on are refused** — every numeric option is checked for a
  finite value in a sane range before a byte is written, so a typo cannot put `ZNaN` in your
  G-code.

---

## 🧪 Slicer support

**This has been developed and tested primarily against OrcaSlicer.** That is the G-code every
measurement in this README was taken from, and the flavour the awkward cases — arc fitting,
`; FEATURE:` markers, Z-hops between wall loops — were found in and fixed against.

PrusaSlicer, SuperSlicer, Bambu Studio and Cura are all handled by the same code path, and
their markers, settings and extrusion modes are covered by tests. But every slicer has its own
habits, and the ones nobody has looked at yet are exactly the ones that bite. A dialect quirk
here does not produce a polite error — it produces G-code that looks fine and prints wrong.

So: **if something comes out wrong, or a warning fires that makes no sense, or your slicer is
not recognised at all, please [raise an issue](https://github.com/erkexzcx/Bricklayers-rust/issues).**
Attach the slicer and version, and a G-code file if you can — a real file is worth more than
any description, and nearly every bug fixed so far was found by reading one.

Before reporting, `--verbose` is worth a look: zero loops found usually means the region
markers were not recognised, which is the most likely way a new dialect fails.

```sh
bricklayers brick --extrusion-multiplier 1.05 --verbose --output /tmp/out.gcode part.gcode
```

---

## ⚠️ Limits, and what has not been tested

### Limits

- **Arachne interlocks less consistently than the classic wall generator.** Arachne varies a
  wall's width along its length, so the number of loops keeps changing, and every change is a
  chance for the stagger to invert relative to the layer below. Measured over one model sliced
  four ways at ~5000 walls each: Arachne inverted the stagger on 32% of layers, and so did
  classic set to inner-outer-inner, against 18% for plain classic in either wall order. It is
  safe, it is just a weaker bond. Use plain classic if you are chasing strength.
- **Two walls interlock half as much as three.** A two-wall region holds one internal loop, so
  it gets one staggered seam against the visible wall where a three-wall region gets two. It is
  bricked, just less. A single wall has no internal perimeter at all and is left alone, with a
  warning.
- **Overhanging sections of walls are left flat.** Slicers label an overhanging stretch of
  perimeter as its own region without saying whether it came from an inner or an outer wall,
  and guessing wrong would raise a *visible* wall — the one failure this exists to avoid — so
  they are all treated as external. Ground truth from a slice with overhang detection turned
  off says 12% of them were really inner wall, which works out at 0.009% of the print. Turning
  detection off changes the result by 46 loops in 15266. Worth knowing, not worth chasing.
- **A wall only two or three layers tall pays for a stagger it never gets.** Starting a raised
  column costs half a layer of extra filament, spread over the two layers above the bed. A
  column that tall is all climb and no column, so it carries the cost with nothing above it to
  bond to. Embossed and engraved detail on a flat face is the usual case. Skipping those needs
  per-contour lookahead across layers, which the two-pass streaming design does not have.
- **Spiral vase does nothing.** One continuously rising wall has no layer boundaries to
  interlock. You get a warning.

### Not tested at all

Not known to be broken. Nobody has run them.

- **Absolute extrusion as a slicer writes it.** The path is validated move for move against a
  real slice converted to absolute mode, but no slicer here emits `M82`. In particular a tool
  change in absolute mode, where each tool keeps its own origin, has never been seen.
- **Every slicer except OrcaSlicer.** See [above](#-slicer-support).
- **`.bgcode` as a slicer writes it at print scale.** The decoder is pinned against Prusa's own
  test files, and a print-scale container repacked from a real slice goes through end to end,
  but those blocks were packed by this tool rather than by PrusaSlicer.

---

## 🚀 Install

Three ways, easiest first. Each one ends the same way: a `bricklayers` binary sitting somewhere
permanent, whose path you hand to your slicer.

### 1. One-liner

```sh
# Linux / macOS
curl -fsSL https://raw.githubusercontent.com/erkexzcx/Bricklayers-rust/main/deploy.sh | bash

# Windows (in PowerShell)
irm https://raw.githubusercontent.com/erkexzcx/Bricklayers-rust/main/deploy.ps1 | iex
```

Downloads the latest release into `~/BrickLayers` (`%USERPROFILE%\BrickLayers` on Windows),
checks it against the published SHA-256 sums, and prints the line to paste into your slicer.

**The same one-liner updates an existing install.** It always resolves the latest release and
overwrites the binary in place, so there is nothing to uninstall first and the path you gave
your slicer does not change. Run it again whenever you want the current version.

### 2. Download by hand

Take the file for your platform from the
[latest release](https://github.com/erkexzcx/Bricklayers-rust/releases/latest) — Linux x86-64
and arm64, macOS on Apple Silicon and Intel, Windows x86-64 and arm64 are published. Rename it to
`bricklayers` (`bricklayers.exe` on Windows), move it somewhere permanent such as
`~/BrickLayers`, and on Linux and macOS mark it executable with `chmod +x bricklayers`.

Check it runs before you wire it into a slicer:

```sh
~/BrickLayers/bricklayers --help
```

If macOS refuses to run it, that is the quarantine flag your browser attaches to downloads —
clear it, or approve the binary in System Settings → Privacy & Security. The one-liner above
downloads with `curl`, which never sets that flag, so this only bites manual downloads.

### 3. Build from source

For any platform not listed above, or if you would rather not run someone else's binary. The
only thing you need is [Rust](https://rustup.rs) — no C compiler, no Python, nothing else:

```sh
git clone https://github.com/erkexzcx/Bricklayers-rust.git
cd Bricklayers-rust
git pull                 # only if you cloned it some time ago and want the latest
cargo build --release
```

The first build takes a minute or two; later ones are seconds. The finished binary is
`target/release/bricklayers` (`bricklayers.exe` on Windows) — copy it wherever you like.

## 🖨️ Use

One line goes into your slicer's **post-processing scripts** field: the binary's absolute path,
a space, then `brick`. Nothing else — the slicer appends the G-code path itself.

```
/home/you/BrickLayers/bricklayers brick --extrusion-multiplier 1.05
```

On Windows, use the full path, and quote it if any folder name contains a space:

```
"C:\Users\you\BrickLayers\bricklayers.exe" brick --extrusion-multiplier 1.05
```

⚠️ **`--extrusion-multiplier` is not a global flow bump.** It rescales *only the loops this
tool raises* — external perimeters, infill, solid layers and the loops left at layer height
all keep the exact flow your slicer gave them. Raised loops are roughly a third of a part's
filament, so `1.05` here adds about 1.5% to its mass where your slicer's flow ratio set to
`1.05` would add 5%. [More on what it is for](#brick), and why `1.05` is a starting point
rather than a recommendation.

**OrcaSlicer / Bambu Studio** and other slicers

1. Open your model.
2. Switch the settings mode to **Advanced** or **Expert** — the
   selector sits at the top of the right-hand settings panel.
3. Go to **Process → Others → Post-processing Scripts**.
4. Paste the one-liner above.
5. Slice.

> 🙈 **The preview will not show the change — and that is normal.** Post-processing runs after
> the slicer has already drawn its preview, so the toolpaths on screen are the *unmodified*
> ones. Every post-processing script in every slicer behaves this way. The file that gets
> exported or sent to the printer *is* processed; only the preview is stale.
>
> To actually look at the result: **export the G-code to a file**, then open that file back in
> the slicer (drag it onto the window, or **File → Open G-code**). Inconvenient, but there is
> no other way.

Plain `.gcode` and binary `.bgcode` are both accepted, and the output keeps whichever format
came in.

### 🛠️ Running it by hand

You do not have to wire it into a slicer at all. Export the G-code as usual, then run the
binary over the file yourself:

```sh
bricklayers brick --extrusion-multiplier 1.05 -o modified.gcode original.gcode
```

`-o` (`--output`) writes a new file and leaves the input untouched, so you can open both in
your slicer and flip between them. Drop `-o` and the file is rewritten in place instead.

Add `-v` to see whether anything actually happened, and what the multiplier costs:

```sh
bricklayers brick --extrusion-multiplier 1.05 -v -o modified.gcode original.gcode
# bricklayers: 247 layers, 1976 internal loops, 988 raised by 0.100 mm
# bricklayers: 7982.6 mm filament, 30.5% of it in raised loops; --extrusion-multiplier 1.05 adds 1.44% to the part
```

Zero raised loops means the file gave the transform nothing to work with — usually a single
wall, or region markers your slicer spells in a way that is not recognised.

**No environment variables needed.** Slicers write their settings into the G-code as comments
(`; layer_height = 0.2`, `; wall_sequence = inner wall/outer wall`), so a file exported by any
mainstream slicer carries everything the transform needs — the layer height and the wall order
are read from there. Use `--layer-height` and `--wall-order` if a file has
no settings block at all, or if what it says is read wrongly.

### `brick`

```
bricklayers brick [OPTIONS] <GCODE>
```

| option | default | |
|---|---|---|
| `--extrusion-multiplier <FACTOR>` | `1.0` | extra flow for raised loops, `1.0` to `1.3` |
| `--reorder-loops` | off | print a layer's unraised loops first, so the nozzle changes height once per layer instead of once per loop — trades ~110 s of Z moves for ~19 m more travel |
| `--layer-height <MM>` | auto | override detection |
| `--wall-order <ORDER>` | `auto` | `auto`, `external-first` or `internal-first` — override the wall order read from the file |

**About `--wall-order`.** Not a preference: it states which order your slicer already prints a
region's walls in, because that decides which end of a wall the loop numbering starts from.
Every mainstream slicer prints the visible wall last, and every mainstream slicer says so in the
file, so `auto` is right almost always. Set it only when detection is wrong — on a 3h print
sliced inner-first, forcing `external-first` took stagger inversions from 472 to 1055.

**About `--reorder-loops`.** Measured on a 3h 19m OrcaSlicer print: 5490 fewer Z-changing moves
(~110 s at that file's Z feedrate) against +19.1 m of travel (+11.9%, ~38 s), for a net saving
of about 0.6% of print time. It is off because that is marginal and the extra travel — plus
printing all of a wall's raised loops consecutively, which changes how the beads cool — has not
been print-tested.

**About `--extrusion-multiplier`.** Pure volume says `1.0`, and that is the default: a raised
column stacks flush like any other, and the two ends of it are metered separately already.
Going above `1.0` compensates for a raised bead being laid against a *step* rather than a flat
plane, where the nozzle cannot press the seam corner closed. That is a physical constant of
your printer and filament rather than something derivable from your G-code, which is why
nothing here picks one for you — the examples above pass `1.05` because it is a sane place to
start, not because it is proven for your printer. Values outside `1.0` to `1.3` are refused:
below `1.0` starves the very seam the raise opens, and above `1.3` a loop takes more than the
gap beside it can hold, so it blobs and the nozzle drags through it.

It applies to raised loops only — external perimeters, infill and unraised loops are never
rescaled — which makes it far cheaper than raising your slicer's global flow: raised loops are
roughly a third of a part's filament, so `1.05` here adds about 1.5% to its mass where a global
`1.05` would add 5%. Run with `-v` for the exact figure on your own file.

**More walls interlock more.** Each internal loop is raised or left flat by its position in the
wall, so a three-wall region gets two staggered seams and a two-wall region gets one — its
single internal loop raised against the external perimeter it was inset from. A solid wall only
a few beads thick is the same case seen from the other side: one internal loop with the visible
wall on *both* sides of it, which is the strongest keying available. Only a single-wall region
has nothing behind the visible wall to raise, and `brick` says so.

External perimeters and the top layer are never touched, so the visible surface is unchanged.
So is the layer laid on the build plate: the column climbs to its offset over the two layers
above it instead. Loops are grouped into walls geometrically, by which loops run beside each
other, and numbering starts at the loop against the external perimeter — a wall gains and loses
loops at the hidden end, so numbering from there would invert the stagger every time the count
changes. Either wall order works, and arc fitting is fine here.

### Shared

| option | |
|---|---|
| `-o, --output <PATH>` | write here instead of overwriting the input |
| `-v, --verbose` | print a summary of what changed |
| `--force` | run even on a file this transform has already processed |

Each transform marks the lines it inserts and refuses to run twice over the same file — a
second pass would stack another shift on the first.

---

## 📄 Licence

GPL-3.0-or-later, matching the project that inspired it.
