# inventory-kicad-models

Portable KiCad component model loading, independent of the inventory application,
filesystem, GPU, and UI. The caller supplies paths, explicit variable mappings and
bytes. `candidates` resolves project/library variables and alternate STEP/VRML
extensions. `embedded` decodes KiCad base64/Zstandard payloads, including pipe-delimited data blocks. `Cache::load` parses
STEP shells or VRML indexed faces into colored triangle meshes and reuses repeated
models, with input/triangle/cache budgets.

STEP surfaces are imported and triangulated by the pure-Rust Truck kernel. STEP
face styles and VRML diffuse colors are preserved. Spline surfaces use a bounded
24-by-24 preview grid rather than unbounded adaptive subdivision; tiny curved
details can therefore be approximate. Parameter fitting is limited to 16 iterations. Boundary fitting uses a bounded
nearest-point fallback so rounded trim edges do not cause curved faces to vanish. The current adapter handles
individual component models; STEP assembly placement hierarchies, VRML external
Inline files, textures, and transparency are not supported. Callers should report
model errors and retain the rest of their scene. KiCad VRML unit conversion (2.54 mm)
is applied by the board renderer, not this generic mesh loader.

```rust
use inventory_kicad_models::Cache;
let mut cache = Cache::default();
// let mesh = cache.load("part.step", &bytes)?;
// Repeated calls with the same key return the same Arc<Mesh>.
```

No environment variables or filesystem paths are read implicitly by this crate.

Tiny spherical fillets use bounded angular sampling even when their radii are
smaller than the preview tolerance.

STEP materials assigned to solid bodies are inherited by their shells; explicit shell and face colours take precedence. This includes body-styled models such as CLN17’s 0606 RGB LED.

STEP boundary curves are converted to bounded preview polylines before kernel
triangulation. Adaptive quarter-point sampling is limited to 512 segments and
8 refinement levels per edge; non-finite samples use finite topological chords
instead of recursively subdividing invalid errors. This prevents pathological
PCurve imports (including RT7272BGSP) from exhausting memory. Endpoints and closed
loops are retained; the tolerance uses the active STEP length unit.
