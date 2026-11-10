These transparent 96 × 96 PNGs are one-time exports of the same SF Symbol names used by the Swift implementation:

- `add.png`: `plus`
- `edit.png`: `pencil`
- `trash.png`: `trash`
- `bulk-add.png`: `plus.square.on.square`
- `notes.png`: `note.text`
- `qr.png`: `qrcode` (15-point semibold, matching Swift’s ToolButton)

The catalog uses the Swift 12-point semibold symbol configuration. PNGs contain a white alpha mask; egui tints them yellow, muted grey, or disabled colour. They are embedded at compile time and cached as textures. Building and running the Rust app does not require AppKit, SF Symbols, Swift, or a macOS export step.

`schematics.png` and `pcb.png` are transparent, full-color navigation assets
resized from the user’s generated icons in `output/icons/`. They are used by the
Requested, Allocated, and Purchase tables’ hover actions and are not tinted as SF Symbols.

The first-level project tree entries reuse `schematics.png` and `pcb.png`, plus
`3d-board.png` prepared from `output/icons/3d-board.png` in the same generated
icon set. These full-color assets are embedded for native and WebAssembly builds;
child schematic rows do not add icons.
