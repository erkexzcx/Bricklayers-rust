# bricklayers — project guidelines

A single Rust binary that post-processes sliced G-code. One transform: `brick`
(raise alternate internal perimeter loops by half a layer height). It runs as a
slicer post-processing script, so it is handed the user's only copy of a file.

## Read this first

**Before changing `src/brick.rs`, `src/scan.rs` or `src/feature.rs`, load
[.github/skills/bricklayers/SKILL.md](skills/bricklayers/SKILL.md).** It holds the
geometric model, the contour-grouping rules with the measurements behind them,
the signals in sliced G-code that look useful and are traps, and
`scripts/audit.py` for verifying output against a real file. Every wrong turn
recorded there started as a confident claim about what a slicer emits.

**Do not assert what a slicer emits — measure it.** Slice a real file, count,
then write the code. Numbers that go into the source or the skill must come from
a real print, not from reading slicer source or reasoning from first principles.

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
cargo run -- brick --extrusion-multiplier 1.05 --verbose --output /tmp/out.gcode ~/Downloads/part.gcode
# bricklayers: 247 layers, 1976 internal loops, 988 raised by 0.100 mm
# bricklayers: 7982.6 mm filament, 30.5% of it in raised loops; --extrusion-multiplier 1.05 adds 1.44% to the part
```

Zero counts mean the region markers were not recognised — grep the input for
`;TYPE:` *and* `; FEATURE:`. Then `grep -n bricklayers /tmp/out.gcode` for the
inserted Z moves, and run the skill's `scripts/audit.py` over the result.

## Architecture

Two passes over the input, and the file is never held in memory: `Source::open`
→ `survey()` (counters only) → `sink()` → `rewrite()` (`BufRead` in, `Write` out).
Peak RSS is flat at ~14 MiB on a 307 MB input.

| file | |
|---|---|
| `src/gcode.rs` | byte-scanner line parser (no regex), `Extruder` M82/M83 mapping, `Lines` reader |
| `src/feature.rs` | Prusa/Orca/Bambu/Cura region markers → one enum |
| `src/footprint.rs` | where a layer's walls sit, as grid cells, so "is anything above this loop?" is a binary search |
| `src/scan.rs` | `Survey`: the single pre-pass |
| `src/brick.rs` | the `brick` transform |
| `src/slicer.rs` | `SLIC3R_*` settings the slicer exports to a post-process script |
| `src/bgcode/` | binary G-code container, heatshrink, meatpack |
| `src/cli.rs`, `src/main.rs` | clap derive, subcommand `brick` |
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
- **A height change must never be a `G1 Z` of its own.** A Z-only move names no
  other axis, so the planner brings the toolhead to a dead stop to run it — on
  the loop's start point, which is the seam, with the nozzle primed. Measured on
  a real PETG part: 679 stops, 13.5 s of standing still, and the stringing to
  show for it. `Pass::carrier`/`ride` put the height on a move the slicer was
  already making, and `Pass::keep` holds a tail of non-extruding lines back
  across the `; FEATURE:` marker so a region's first loop has one to ride.
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
- **The layer laid on the bed is never raised, and a column climbs to its offset
  over `RAMP` (2) layers.** A bead on the plate is not pressed by the nozzle, so
  the flow a raise needs spreads sideways instead of building height — it filled
  in a Benchy's bottom nameplate, which is exactly one layer deep.
  `extrusion_factor` is one formula,
  `(layer_height + rise(k) - rise(k-1)) / layer_height`, covering the bed, the
  climb, the steady state and the cap. Do not special-case any of them back.
- **Every number that reaches the nozzle is checked at the boundary.** `cli::within`
  refuses a value that is not finite and in range, and a layer height is filtered
  by `scan::is_a_height` at every place it can arrive: CLI, slicer environment,
  bgcode metadata and the survey.

## Conventions

- Doc comments explain *why*, and cite the measurement when a constant encodes
  one — see `MAX_LOOP_GAP`, `PROBES` and `RAMP` in `src/brick.rs`. Do not add
  comments that restate the next line.
- No `unsafe`. Errors are `crate::Result` over a `thiserror` enum; `unwrap` only
  in tests and where a comment shows it cannot fire.
- Dependencies are deliberately few (`clap`, `crc32fast`, `flate2`, `thiserror`).
  Do not add one without saying what it replaces.
- Unit tests live in `#[cfg(test)]` modules next to the code; behaviour that
  spans the binary goes in `tests/`.
- Commit subjects are imperative and scoped where it helps:
  `brick: number a wall's loops from the visible side`.
- Anything a slicer does that surprised you is a warning, never a hard failure —
  the user's print is already in progress.
