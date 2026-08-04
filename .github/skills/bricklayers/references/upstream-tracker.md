# Upstream tracker

Triage of [TengerTechnologies/Bricklayers](https://github.com/TengerTechnologies/Bricklayers),
the Python project this is an independent reimplementation of. 81 entries at the
time of review: 58 issues and 23 pull requests, 256 comments.

Read this before treating any upstream report as a defect here. Several of the
most-repeated complaints do not apply, and one of the largest symptom clusters
was refuted by measurement.

## Fixed here

| Report | What it was | Where |
|---|---|---|
| First layer height ≠ layer height | `1.5×` flow constant assumes they are equal; a thick first layer comes out starved. The most repeated workaround on the tracker is "set first layer height = layer height". | `extrusion_factor` |
| Lone internal loop raised | A wall with one internal loop has nothing to stagger against. shapebox raised 193 of 194 such loops. | `number_loops` |
| File mode not preserved | In-place rewrite is a fresh file renamed over the original, so 0600 came back 0644. | `Source::sink` |
| No re-run guard | Second pass compounds the shift. | `Error::AlreadyProcessed`, `--force` |
| Wall-count parity flips | Numbering from the first-printed loop inverts the stagger whenever the wall gains a loop. | `number_loops` |
| Binary G-code unsupported | Upstream scripts fail outright on `.bgcode`. | `src/bgcode/` |
| Layer height guessing | Slicers export the whole config into the environment. | `src/slicer.rs` |

## Refuted by measurement

| Report | Evidence |
|---|---|
| **Outer-wall gaps / wall-order dependency** — the largest cluster on the tracker (5 issues, ~30 comments), including the original Benchy that reproduced it in Python | **0 of 155 058** external perimeter extrusions are emitted while the nozzle is raised, across five real files. The flush-on-marker reset handles it. Does not apply here. |
| Arc fitting breaks flow rescaling | An arc's `E` word is rewritten like any other: `E.08496` → `E0.12744`, exactly the 1.5× the first layer asks for. The real arc defect was loop *detection*, not flow. |

## Not reproduced

Needs a fixture or a targeted trace, not a statistic. Do not act on these
without evidence.

| Item | Blocker |
|---|---|
| Global last-layer test / taper on unraised loops | Needs a multi-object or capped-boss fixture |
| Scarf seams | Both attachments are already Python-processed; needs a raw re-slice |
| Multi-object / print-by-object | No raw fixture |
| Stringing around Z transitions | Physical symptom, not statically detectable |
| Line widths on reimport | Needs a slicer to observe the reported value |
| Z-hop poisoning the layer base | 630 rises / 417 falls on a Benchy — inconclusive |
| Trailing travel under `--reorder-loops` | Not yet exercised |

## Open design questions

- A `--start-layer` option, requested more than once.

## Settled

- **Should `--reorder-loops` default on? No.** Measured on `orig-maxwalls.gcode`
  (3h 19m print): it removes 5490 Z-changing moves (44491 → 39001, ~110 s at the
  file's own `F300` Z feedrate) and adds 19.1 m of travel (160.7 → 179.8 m,
  +11.9%, ~38 s at `travel_speed = 500`). Net ~0.6% of print time, against
  untested stringing and an untested change to how a wall's beads cool. Stays
  off; the numbers are in the README.

## Investigation debt

- 23 pull request diffs unread.
- ~150 comments across 37 issue threads unread.

## Process notes

The first two passes over this tracker both returned exactly five items, in
both cases because the output was being shaped rather than the evidence
enumerated. The honest counts were roughly 13 and 7. Enumerate as data first,
state the raw count, then rank — and do not cap the list.

Reproduction beat reasoning every time. Two conclusions drawn from a
hand-written single-layer fixture were both wrong, because layer 0 is also the
last layer and never gets raised. Use real slicer output.
