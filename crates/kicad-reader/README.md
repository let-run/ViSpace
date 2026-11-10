# inventory-kicad

Standalone, read-only KiCad 6+ project, schematic and PCB reader. The JSON and
S-expression parsers are implemented here from scratch; serde is used only for
serializing the resulting data structures. There are no KiCad parser crates,
KiCad installation requirements, filesystem dependencies, or platform APIs.

```rust,no_run
let bundle = inventory_kicad::load_project("design.kicad_pro", |file| {
    std::fs::read_to_string(file)
        .map_err(|error| inventory_kicad::Error::new(error.to_string()))
})?;
for node in bundle.nodes() {
    println!("{}: {}", node.instance_path, node.file);
}
# Ok::<(), inventory_kicad::Error>(())
```

`load_project` eagerly includes the board for standalone callers. For interactive
applications, `load_schematics` discovers board availability without parsing it,
and `load_board` loads only the board and its project settings on demand. Discovery
accepts an empty string for an existing board, so callers can use metadata instead
of reading its contents.

The caller supplies file access, which can be a filesystem, uploaded browser
files, or an in-memory map. `Project::parse`, `Schematic::parse`, `Pcb::parse`, `syntax::parse`,
and `json::parse` can also be used independently. `Schematic::primitives` is a
renderer-neutral scene in millimetres with sheet coordinates (Y down).

Supports project text variables, relative paths and `${KIPRJMOD}`, hierarchical
sheets, repeated sheet instances, cached symbol definitions, symbol units and
inheritance, rotation/mirroring, pins, wires, buses, junctions, labels, text,
rectangles, polylines, circles, arcs, cubic Bezier curves, and embedded PNG/JPEG images (including physical resolution and scale). Reference property
overrides are retained per sheet instance. Missing child files and cycles are
reported on tree nodes without discarding the rest of the project.

This is a schematic navigation renderer, not a KiCad editor or electrical rule
checker. Legacy `.pro`/`.sch` files are rejected. Unsupported drawing objects and invalid images are reported as warnings. Label outlines,
text metrics, and specialized pin decorations may differ from KiCad. External
symbol libraries are not resolved: modern schematics normally embed their
symbol definitions in `lib_symbols`.

Input bounds: 32 MiB/file, 128 syntax nesting levels, 2 million syntax nodes,
64 MiB of schematic sources/project, 128 MiB of decoded images/schematic (8192 pixels/axis and 64 MiB decode allocation/image), and 512 hierarchical sheet instances
(maximum hierarchy depth 32). No source is executed or written.

Run `cargo test -p inventory-kicad` from the workspace. The `inspect` example
prints a hierarchy and primitive counts for a project or standalone schematic.
The fixtures in this crate are small original test designs.

Format references: [schematics](https://dev-docs.kicad.org/en/file-formats/sexpr-schematic/),
[symbol libraries](https://dev-docs.kicad.org/en/file-formats/sexpr-symbol-lib/),
[S-expression conventions](https://dev-docs.kicad.org/en/file-formats/sexpr-intro/).

`Schematic::components` exposes physical symbol properties, BOM/DNP flags, and
per-instance references independently of drawing primitives. Consumers can use
these to derive a BOM without parsing display text or counting unit suffixes.

## PCB scenes

`Pcb::parse(source)` returns a platform-independent board scene with layer names,
IDs and types; layer-tagged paths/text; drill contours; bounds; and warnings.
`load_project` also reads the optional same-basename `.kicad_pcb` file into
`Bundle::pcb`. Missing boards do not prevent schematic loading; malformed boards
report `pcb_error`. Tracks, modern arcs, footprints, rotated circle/oval/roundrect/
rectangular/trapezoid pads, custom pad primitives, vias, and saved zone fills are
supported. Filled polygons (up to 32,768 vertices) include triangle indices so
renderers do not need to triangulate on every frame. No source files are modified.
The viewer is 2D; dimensions, text boxes, 3D models, and legacy arc encodings are
not supported. Unknown graphic forms produce warnings. Zone fills use the saved
geometry, without running KiCad's connectivity/clearance or refill engine.

PCB metadata also exposes pad numbers/net names, track net names, and pad clearance
contours. `Pcb::apply_project_settings` resolves net-class clearances from project
JSON, including exact assignments, wildcard patterns, class priority, and Default.
Local pad and footprint overrides take precedence; pre-9 zero values inherit while
KiCad 9+ explicit zero values override. Custom pad clearance contours use an
approximate convex hull. Conditional `.kicad_dru` rules are not evaluated.

Project bundles share parsed schematics and PCB boards through immutable `Arc`
references. Cloning a bundle or passing a board to a renderer does not duplicate
all geometry or embedded image data; the serialized project representation is
unchanged.

`Pcb::footprint_list` exposes designators, placement layers, origins, and body
bounds; `PcbItem::reference` groups drawing items with their parent footprint for
side-aware picking and schematic/PCB cross-selection. Body bounds exclude text.

## Net classes

`Project::net_classes` reads `.kicad_pro` class definitions, explicit net
assignments, legacy class net lists, and wildcard assignment patterns.
`NetClasses::resolve` groups a supplied net-name set, retains empty classes, and
places unassigned nets in Default. Nets with multiple assignments belong to each
matching class. The reader remains independent of filesystem and rendering APIs.

`nets::group_net_names` builds navigation groups from complete `_N`/`_P` or
`_M`/`_P` name pairs, retaining original net names and hierarchical prefixes.
Unpaired names remain individual entries; grouping does not change connectivity.
