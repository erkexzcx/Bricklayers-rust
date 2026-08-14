---
name: Bug report
about: Something is wrong with the output, or the tool does not do what it should
title: ''
labels: bug
assignees: ''
---

## Which transform

- [ ] `--bricks` (BrickLayers)
- [ ] `--zaa` (Z anti-aliasing)
- [ ] both together

## What happened

<!-- What you expected, and what you got instead. -->

## Files (required for any G-code issue)

If this is about how the G-code is read or rewritten, the issue cannot be
investigated without the files. Attach them by dragging them into this box — zip them
first if GitHub rejects the extension.

- [ ] **Original G-code** — straight out of the slicer, before corbel touched it
- [ ] **Modified G-code** — the output of this tool (use `--output out.gcode` so the original stays intact)
- [ ] **Screenshots** — the slicer preview or the printed part, if the problem is visible

## Command

```
corbel --bricks --zaa --verbose --output out.gcode part.gcode
```

<!-- Paste the exact command you ran and its output. -->

## Versions

- corbel: <!-- `corbel --version` -->
- Slicer: <!-- e.g. PrusaSlicer 2.8.1, OrcaSlicer 2.2.0, Cura 5.7 -->
- OS: <!-- e.g. Windows 11, Ubuntu 24.04, macOS 15 -->
- Printer / filament: <!-- if relevant -->
