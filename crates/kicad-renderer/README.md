# inventory-kicad-renderer

Reusable read-only KiCad board scene builder and retained wgpu renderer. There are
no inventory/project-catalog dependencies or filesystem operations. Inputs are an
`inventory_kicad::Pcb` plus a map of original KiCad model-reference strings to
`ModelAsset { name, bytes }`. Run `build` on a background worker and supply its
cancellation predicate. Output `Arc<Scene>` is serializable for Web Worker transport.

The scene includes substrate thickness, closed board outlines/cutouts, plated drill
walls, pads, vias, outer-layer traces and copper pours under the solder-mask
appearance, silkscreen paths and text, and transformed component models. Model orientation follows KiCad’s clockwise
Z/Y/X rotation convention, including back-side placements. Copper
strokes have rounded caps/joins and drill openings are cut out of their geometry.
Outer copper is retained as separate `F.Cu`/`B.Cu` meshes with upper/lower faces
and boundary walls, using a default 0.035 mm copper thickness. Coverage is unioned and drills are subtracted at the polygon level before
extrusion. Walls follow only the resulting contours, including holes; internal
triangulation edges never create walls. Vertices are indexed to limit memory. This is mesh geometry,
not STEP/B-rep output. The green mask appearance is still baked into the material;
a separate transparent mask and environment reflections are not implemented.
KiCad render-cache polygons preserve exact outline-font silkscreen geometry,
including font weight, placement and mirroring. Text without saved outlines uses
bundled Hack outlines cached per glyph during preparation. Matched upper/lower studio lights keep either side readable.
`draw_with_brightness` adjusts illumination without rebuilding meshes. Repeated
models share a mesh and use separate placement transforms. Unavailable models are reported and omitted; no placeholder boxes are generated.

The default `egui` feature supplies:

- `register(&RenderState, &egui::Context)` once during renderer setup;
- `Camera::fit(&Scene)` and `draw(&mut Ui, &Arc<Scene>, &mut Camera)`;
- `release(&Context, &Scene)` when closing/replacing a scene.

Drag to orbit, Shift/right-drag to pan, wheel/pinch to zoom, double-click to fit.
Geometry/instance buffers persist across camera changes. Depth-tested lighting is
rendered with reversed-Z `Depth32Float` offscreen to avoid thin-layer z-fighting; unchanged frames reuse the finished texture. `release` frees
the GPU scene and its color/depth targets.

Use `default-features = false` for the scene-building API without egui/wgpu.
Both APIs compile for desktop and `wasm32-unknown-unknown`. The application owns
native file I/O, browser directory permissions, model-library preferences, and
worker scheduling.


`Selection` accepts a net name or a set of designators. `build_selection` prepares
only the selected copper, including internal layers and via barrels, on a worker.
Saved stackup centre depths are used when present; otherwise internal depths are
estimated from copper layer order and board thickness. Colours match the PCB view.
`draw_selected` retains the base model buffers and renders an independent X-ray
overlay, keeping internal selected copper visible through the muted greyscale
context. Context strength affects shading; it is not a physically transparent
solder-mask material. Selected component instances retain their original colours.
`pick` supports model-bound picking and facing-surface net picking; the application
owns search, cross-view selection and background job cancellation.

`cache::PreparedCache` provides a portable, versioned prepared-scene cache.
`matching_scene` checks board/model content fingerprints; `prepare` reuses unchanged
component geometry on a miss. `encode`/`decode` retain model indices into the scene
rather than duplicating meshes. The application owns compression, background work,
and atomic persistence to `cache::CACHE_NAME` (`.vspace.cache`). The same bytes
are usable on desktop and wasm; decoding validates GPU indices and finite values.
