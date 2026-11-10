//! Union coverage within each inactive PCB layer, then apply opacity once.
//! MAX blending prevents track caps, pads, and segments from accumulating alpha.
use super::*;
use egui_wgpu::{CallbackResources, CallbackTrait, ScreenDescriptor, wgpu};
use std::collections::BTreeMap;
use wgpu::util::DeviceExt;

pub(super) fn register(state: &egui_wgpu::RenderState) {
    let gpu = Gpu::new(&state.device, state.target_format);
    state.renderer.write().callback_resources.insert(gpu);
}
pub(super) fn callback(
    ui: &egui::Ui,
    rect: egui::Rect,
    layers: BTreeMap<String, Vec<egui::Shape>>,
    opacity: f32,
) -> egui::Shape {
    if opacity <= 0. || layers.is_empty() {
        return egui::Shape::Noop;
    }
    let meshes = layers
        .into_values()
        .map(|shapes| geometry_meshes(ui, shapes, rect, ui.ctx().pixels_per_point()))
        .collect();
    egui_wgpu::Callback::new_paint_callback(rect, Layers { meshes, opacity }).into()
}
fn geometry_meshes(
    ui: &egui::Ui,
    shapes: Vec<egui::Shape>,
    rect: egui::Rect,
    pixels_per_point: f32,
) -> Vec<egui::Mesh> {
    let mut options = ui.ctx().tessellation_options(|o| *o);
    // These pipelines render geometry, not atlas textures. Generate actual circle
    // triangles instead of egui's small pre-rasterized font-atlas disc quads.
    options.prerasterized_discs = false;
    egui::epaint::Tessellator::new(pixels_per_point, options, [1, 1], vec![])
        .tessellate_shapes(
            shapes
                .into_iter()
                .map(|shape| egui::epaint::ClippedShape {
                    clip_rect: rect,
                    shape,
                })
                .collect(),
        )
        .into_iter()
        .filter_map(|p| {
            if let egui::epaint::Primitive::Mesh(mesh) = p.primitive {
                Some(mesh)
            } else {
                None
            }
        })
        .collect()
}
struct Layers {
    meshes: Vec<Vec<egui::Mesh>>,
    opacity: f32,
}
struct Target {
    _texture: wgpu::Texture,
    view: wgpu::TextureView,
    bind: wgpu::BindGroup,
}
struct Gpu {
    mask_pipeline: wgpu::RenderPipeline,
    composite_pipeline: wgpu::RenderPipeline,
    paint_pipeline: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
    parameters: wgpu::Buffer,
    size: [u32; 2],
    mask: Option<Target>,
    output: Option<Target>,
}
const SHADER: &str = r#"
struct Vertex { @builtin(position) position: vec4<f32>, @location(0) color:vec4<f32> }
@vertex fn geometry(@location(0) p:vec2<f32>, @location(1) c:u32)->Vertex {
    var out:Vertex;out.position=vec4<f32>(p,0.,1.);out.color=unpack4x8unorm(c);return out;
}
@fragment fn coverage(in:Vertex)->@location(0) vec4<f32> {return in.color;}
@group(0) @binding(0) var image:texture_2d<f32>;
@group(0) @binding(1) var<uniform> parameters:vec4<f32>;
@vertex fn fullscreen(@builtin(vertex_index) i:u32)->@builtin(position) vec4<f32> {
    let p=array<vec2<f32>,3>(vec2<f32>(-1.,-1.),vec2<f32>(3.,-1.),vec2<f32>(-1.,3.));
    return vec4<f32>(p[i],0.,1.);
}
@fragment fn composite(@builtin(position) p:vec4<f32>)->@location(0) vec4<f32> {
    return textureLoad(image,vec2<i32>(p.xy),0)*parameters.x;
}
@fragment fn paint(@builtin(position) p:vec4<f32>)->@location(0) vec4<f32> {
    return textureLoad(image,vec2<i32>(p.xy),0);
}
@fragment fn paint_linear(@builtin(position) p:vec4<f32>)->@location(0) vec4<f32> {
    let c=textureLoad(image,vec2<i32>(p.xy),0);
    let rgb=select(c.rgb/12.92,pow((c.rgb+vec3<f32>(0.055))/1.055,vec3<f32>(2.4)),c.rgb>vec3<f32>(0.04045));
    return vec4<f32>(rgb,c.a);
}
"#;
impl Gpu {
    fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("PCB layer compositor"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("PCB layer texture"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });
        let empty = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("PCB coverage layout"),
            bind_group_layouts: &[],
            push_constant_ranges: &[],
        });
        let textured = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("PCB compositor layout"),
            bind_group_layouts: &[&layout],
            push_constant_ranges: &[],
        });
        let max = wgpu::BlendComponent {
            src_factor: wgpu::BlendFactor::One,
            dst_factor: wgpu::BlendFactor::One,
            operation: wgpu::BlendOperation::Max,
        };
        let vertex_attributes = wgpu::vertex_attr_array![0=>Float32x2,1=>Uint32];
        let vertex_buffers = [wgpu::VertexBufferLayout {
            array_stride: 12,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &vertex_attributes,
        }];
        let pipeline = |layout: &wgpu::PipelineLayout,
                        vertex: &str,
                        fragment: &str,
                        format,
                        blend,
                        geometry: bool| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(fragment),
                layout: Some(layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some(vertex),
                    compilation_options: Default::default(),
                    buffers: if geometry { &vertex_buffers } else { &[] },
                },
                primitive: Default::default(),
                depth_stencil: None,
                multisample: Default::default(),
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some(fragment),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format,
                        blend: Some(blend),
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                multiview: None,
                cache: None,
            })
        };
        let mask_pipeline = pipeline(
            &empty,
            "geometry",
            "coverage",
            wgpu::TextureFormat::Rgba8Unorm,
            wgpu::BlendState {
                color: max,
                alpha: max,
            },
            true,
        );
        let composite_pipeline = pipeline(
            &textured,
            "fullscreen",
            "composite",
            wgpu::TextureFormat::Rgba8Unorm,
            wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING,
            false,
        );
        let paint_pipeline = pipeline(
            &textured,
            "fullscreen",
            if format.is_srgb() {
                "paint_linear"
            } else {
                "paint"
            },
            format,
            wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING,
            false,
        );
        let parameters = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("PCB layer opacity"),
            size: 16,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        Self {
            mask_pipeline,
            composite_pipeline,
            paint_pipeline,
            layout,
            parameters,
            size: [0, 0],
            mask: None,
            output: None,
        }
    }
    fn resize(&mut self, device: &wgpu::Device, size: [u32; 2]) {
        if self.size == size {
            return;
        }
        self.size = size;
        let target = || {
            let texture = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("PCB layer coverage"),
                size: wgpu::Extent3d {
                    width: size[0],
                    height: size[1],
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8Unorm,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                    | wgpu::TextureUsages::TEXTURE_BINDING
                    | wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            });
            let view = texture.create_view(&Default::default());
            let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("PCB compositing texture"),
                layout: &self.layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(&view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: self.parameters.as_entire_binding(),
                    },
                ],
            });
            Target {
                _texture: texture,
                view,
                bind,
            }
        };
        self.mask = Some(target());
        self.output = Some(target());
    }
}
fn attachment(view: &wgpu::TextureView, clear: bool) -> wgpu::RenderPassColorAttachment<'_> {
    wgpu::RenderPassColorAttachment {
        view,
        resolve_target: None,
        depth_slice: None,
        ops: wgpu::Operations {
            load: if clear {
                wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT)
            } else {
                wgpu::LoadOp::Load
            },
            store: wgpu::StoreOp::Store,
        },
    }
}
impl CallbackTrait for Layers {
    fn prepare(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        screen: &ScreenDescriptor,
        encoder: &mut wgpu::CommandEncoder,
        resources: &mut CallbackResources,
    ) -> Vec<wgpu::CommandBuffer> {
        let Some(gpu) = resources.get_mut::<Gpu>() else {
            return vec![];
        };
        gpu.resize(device, screen.size_in_pixels);
        let bytes: Vec<_> = [self.opacity, 0., 0., 0.]
            .into_iter()
            .flat_map(f32::to_ne_bytes)
            .collect();
        queue.write_buffer(&gpu.parameters, 0, &bytes);
        let mask = gpu.mask.as_ref().unwrap();
        let output = gpu.output.as_ref().unwrap();
        {
            let _pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("PCB clear layer composite"),
                color_attachments: &[Some(attachment(&output.view, true))],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
        }
        if self.opacity <= 0. {
            return vec![];
        }
        let width = screen.size_in_pixels[0] as f32 / screen.pixels_per_point;
        let height = screen.size_in_pixels[1] as f32 / screen.pixels_per_point;
        for meshes in &self.meshes {
            let mut vertices = vec![];
            let mut indices = vec![];
            let mut count = 0u32;
            for mesh in meshes {
                for vertex in &mesh.vertices {
                    vertices.extend_from_slice(&(vertex.pos.x / width * 2. - 1.).to_ne_bytes());
                    vertices.extend_from_slice(&(1. - vertex.pos.y / height * 2.).to_ne_bytes());
                    vertices.extend_from_slice(&vertex.color.to_array());
                }
                for index in &mesh.indices {
                    indices.extend_from_slice(&(index + count).to_ne_bytes());
                }
                count += mesh.vertices.len() as u32;
            }
            if indices.is_empty() {
                continue;
            }
            let vb = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("PCB union vertices"),
                contents: &vertices,
                usage: wgpu::BufferUsages::VERTEX,
            });
            let ib = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("PCB union indices"),
                contents: &indices,
                usage: wgpu::BufferUsages::INDEX,
            });
            {
                let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("PCB layer MAX coverage"),
                    color_attachments: &[Some(attachment(&mask.view, true))],
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                });
                pass.set_pipeline(&gpu.mask_pipeline);
                pass.set_vertex_buffer(0, vb.slice(..));
                pass.set_index_buffer(ib.slice(..), wgpu::IndexFormat::Uint32);
                pass.draw_indexed(0..(indices.len() / 4) as u32, 0, 0..1);
            }
            {
                let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("PCB apply layer opacity once"),
                    color_attachments: &[Some(attachment(&output.view, false))],
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                });
                pass.set_pipeline(&gpu.composite_pipeline);
                pass.set_bind_group(0, &mask.bind, &[]);
                pass.draw(0..3, 0..1);
            }
        }
        vec![]
    }
    fn paint(
        &self,
        _info: egui::PaintCallbackInfo,
        pass: &mut wgpu::RenderPass<'static>,
        resources: &CallbackResources,
    ) {
        if let Some(gpu) = resources.get::<Gpu>()
            && let Some(output) = &gpu.output
        {
            pass.set_pipeline(&gpu.paint_pipeline);
            pass.set_bind_group(0, &output.bind, &[]);
            pass.draw(0..3, 0..1);
        }
    }
}

// Orthographic-camera experiment: scene vertices stay in board coordinates and
// GPU buffers survive zoom/pan. Only the camera uniform changes on those frames.
use inventory_kicad::{Pcb, PcbShape, Point};
use std::sync::Arc;
const SCENE_SCALE: f32 = 64.;
pub(super) struct SceneCache {
    board: Arc<Pcb>,
    active: String,
    hidden: BTreeSet<String>,
    opacity: f32,
    net_colors: BTreeMap<String, [u8; 3]>,
    scene: Arc<WorldScene>,
}
pub(super) struct WorldScene {
    pub(super) meshes: Vec<Vec<egui::Mesh>>,
}
pub(super) fn cached_scene(
    ui: &egui::Ui,
    cache: &mut Option<SceneCache>,
    board: &Arc<Pcb>,
    active: &str,
    hidden: &BTreeSet<String>,
    opacity: f32,
) -> Arc<WorldScene> {
    colored_scene(ui, cache, board, active, hidden, opacity, &BTreeMap::new())
}
pub(super) fn colored_scene(
    ui: &egui::Ui,
    cache: &mut Option<SceneCache>,
    board: &Arc<Pcb>,
    active: &str,
    hidden: &BTreeSet<String>,
    opacity: f32,
    net_colors: &BTreeMap<String, [u8; 3]>,
) -> Arc<WorldScene> {
    if let Some(cache) = cache
        && Arc::ptr_eq(&cache.board, board)
        && cache.active == active
        && cache.hidden == *hidden
        && cache.opacity == opacity
        && cache.net_colors == *net_colors
    {
        return cache.scene.clone();
    }
    let mut layers: BTreeMap<String, Vec<egui::Shape>> = BTreeMap::new();
    for item in &board.items {
        if super::cad_gpu::cancelled() {
            break;
        }
        let visible: Vec<_> = item
            .layers
            .iter()
            .filter(|l| !hidden.contains(*l))
            .collect();
        if visible.is_empty() {
            continue;
        }
        let is_active = visible.iter().any(|l| l.as_str() == active);
        let layer = if is_active { active } else { visible[0] };
        let mut color =
            super::pcb::layer_color(layer).gamma_multiply(if is_active { 1. } else { opacity });
        if layer.ends_with(".Cu")
            && super::pcb::is_net_copper(item)
            && let Some(c) = net_colors.get(&item.net)
        {
            color = Color32::from_rgb(c[0], c[1], c[2]).gamma_multiply(if is_active {
                1.
            } else {
                opacity
            });
        }
        if item.zone {
            color = color.gamma_multiply(0.4);
        }
        let PcbShape::Path {
            points,
            width,
            filled,
            triangles,
        } = &item.shape
        else {
            continue;
        };
        let points: Vec<_> = points
            .iter()
            .map(|p| egui::pos2(p.x as f32 * SCENE_SCALE, p.y as f32 * SCENE_SCALE))
            .collect();
        let shapes = layers.entry(layer.to_owned()).or_default();
        if *filled {
            let mut mesh = egui::Mesh::default();
            for p in points {
                mesh.colored_vertex(p, color);
            }
            for t in triangles {
                mesh.add_triangle(t[0], t[1], t[2]);
            }
            shapes.push(mesh.into());
        } else {
            shapes.extend(super::pcb::rounded_path(
                points,
                egui::Stroke::new(*width as f32 * SCENE_SCALE, color),
            ));
        }
    }
    let active_shapes = layers.remove(active);
    let mut ordered: Vec<_> = layers.into_values().collect();
    if let Some(shapes) = active_shapes {
        ordered.push(shapes);
    }
    let meshes = ordered
        .into_iter()
        .map(|shapes| {
            geometry_meshes(ui, shapes, egui::Rect::EVERYTHING, 1.)
                .into_iter()
                .map(|mut mesh| {
                    for v in &mut mesh.vertices {
                        v.pos.x /= SCENE_SCALE;
                        v.pos.y /= SCENE_SCALE;
                    }
                    mesh
                })
                .collect()
        })
        .collect();
    let scene = Arc::new(WorldScene { meshes });
    *cache = Some(SceneCache {
        board: board.clone(),
        active: active.into(),
        hidden: hidden.clone(),
        opacity,
        net_colors: net_colors.clone(),
        scene: scene.clone(),
    });
    scene
}
pub(super) fn world_callback(
    rect: egui::Rect,
    scene: Arc<WorldScene>,
    center: Point,
    scale: f32,
) -> egui::Shape {
    egui_wgpu::Callback::new_paint_callback(
        rect,
        WorldLayers {
            scene,
            rect,
            center,
            scale,
        },
    )
    .into()
}
struct WorldLayers {
    scene: Arc<WorldScene>,
    rect: egui::Rect,
    center: Point,
    scale: f32,
}
struct WorldBuffer {
    vertices: wgpu::Buffer,
    indices: wgpu::Buffer,
    count: u32,
}
struct WorldGpu {
    pipeline: wgpu::RenderPipeline,
    camera: wgpu::Buffer,
    bind: wgpu::BindGroup,
    cached: Option<Arc<WorldScene>>,
    buffers: Vec<WorldBuffer>,
    uploads: usize,
}
const WORLD_SHADER: &str = r#"
struct Camera { transform:vec4<f32>, viewport:vec4<f32> }
@group(0) @binding(0) var<uniform> camera:Camera;
struct Vertex { @builtin(position) position:vec4<f32>, @location(0) color:vec4<f32> }
@vertex fn world(@location(0) p:vec2<f32>,@location(1) c:u32)->Vertex {
    let screen=(p-camera.transform.xy)*camera.transform.z+camera.viewport.xy;
    var out:Vertex;out.position=vec4<f32>(screen.x/camera.viewport.z*2.-1.,1.-screen.y/camera.viewport.w*2.,0.,1.);out.color=unpack4x8unorm(c);return out;
}
@fragment fn coverage(in:Vertex)->@location(0) vec4<f32>{return in.color;}
"#;
impl WorldGpu {
    fn new(device: &wgpu::Device) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("PCB orthographic camera"),
            source: wgpu::ShaderSource::Wgsl(WORLD_SHADER.into()),
        });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("PCB camera layout"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let camera = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("PCB orthographic camera"),
            size: 32,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("PCB camera"),
            layout: &layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: camera.as_entire_binding(),
            }],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("PCB world scene layout"),
            bind_group_layouts: &[&layout],
            push_constant_ranges: &[],
        });
        let max = wgpu::BlendComponent {
            src_factor: wgpu::BlendFactor::One,
            dst_factor: wgpu::BlendFactor::One,
            operation: wgpu::BlendOperation::Max,
        };
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("PCB persistent world geometry"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("world"),
                compilation_options: Default::default(),
                buffers: &[wgpu::VertexBufferLayout {
                    array_stride: 12,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &wgpu::vertex_attr_array![0=>Float32x2,1=>Uint32],
                }],
            },
            primitive: Default::default(),
            depth_stencil: None,
            multisample: Default::default(),
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("coverage"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: wgpu::TextureFormat::Rgba8Unorm,
                    blend: Some(wgpu::BlendState {
                        color: max,
                        alpha: max,
                    }),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview: None,
            cache: None,
        });
        Self {
            pipeline,
            camera,
            bind,
            cached: None,
            buffers: vec![],
            uploads: 0,
        }
    }
    fn upload(&mut self, device: &wgpu::Device, scene: &Arc<WorldScene>) {
        if self
            .cached
            .as_ref()
            .is_some_and(|old| Arc::ptr_eq(old, scene))
        {
            return;
        }
        self.buffers.clear();
        for meshes in &scene.meshes {
            let (mut vertices, mut indices, mut count) = (vec![], vec![], 0u32);
            for mesh in meshes {
                for v in &mesh.vertices {
                    vertices.extend_from_slice(&v.pos.x.to_ne_bytes());
                    vertices.extend_from_slice(&v.pos.y.to_ne_bytes());
                    vertices.extend_from_slice(&v.color.to_array());
                }
                for i in &mesh.indices {
                    indices.extend_from_slice(&(i + count).to_ne_bytes());
                }
                count += mesh.vertices.len() as u32;
            }
            if indices.is_empty() {
                continue;
            }
            self.buffers.push(WorldBuffer {
                vertices: device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("PCB persistent vertices"),
                    contents: &vertices,
                    usage: wgpu::BufferUsages::VERTEX,
                }),
                indices: device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("PCB persistent indices"),
                    contents: &indices,
                    usage: wgpu::BufferUsages::INDEX,
                }),
                count: (indices.len() / 4) as u32,
            });
        }
        self.cached = Some(scene.clone());
        self.uploads += 1;
    }
}
impl CallbackTrait for WorldLayers {
    fn prepare(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        screen: &ScreenDescriptor,
        encoder: &mut wgpu::CommandEncoder,
        resources: &mut CallbackResources,
    ) -> Vec<wgpu::CommandBuffer> {
        let mut world = resources
            .remove::<WorldGpu>()
            .unwrap_or_else(|| WorldGpu::new(device));
        let gpu = resources.get_mut::<Gpu>().unwrap();
        gpu.resize(device, screen.size_in_pixels);
        world.upload(device, &self.scene);
        let values = [
            self.center.x as f32,
            self.center.y as f32,
            self.scale,
            0.,
            self.rect.center().x,
            self.rect.center().y,
            screen.size_in_pixels[0] as f32 / screen.pixels_per_point,
            screen.size_in_pixels[1] as f32 / screen.pixels_per_point,
        ];
        queue.write_buffer(
            &world.camera,
            0,
            &values
                .into_iter()
                .flat_map(f32::to_ne_bytes)
                .collect::<Vec<_>>(),
        );
        queue.write_buffer(
            &gpu.parameters,
            0,
            &[1f32, 0., 0., 0.]
                .into_iter()
                .flat_map(f32::to_ne_bytes)
                .collect::<Vec<_>>(),
        );
        let mask = gpu.mask.as_ref().unwrap();
        let output = gpu.output.as_ref().unwrap();
        {
            let _pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("PCB camera clear output"),
                color_attachments: &[Some(attachment(&output.view, true))],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
        }
        for buffer in &world.buffers {
            {
                let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("PCB world layer"),
                    color_attachments: &[Some(attachment(&mask.view, true))],
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                });
                pass.set_pipeline(&world.pipeline);
                pass.set_bind_group(0, &world.bind, &[]);
                pass.set_vertex_buffer(0, buffer.vertices.slice(..));
                pass.set_index_buffer(buffer.indices.slice(..), wgpu::IndexFormat::Uint32);
                pass.draw_indexed(0..buffer.count, 0, 0..1);
            }
            {
                let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("PCB world composite"),
                    color_attachments: &[Some(attachment(&output.view, false))],
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                });
                pass.set_pipeline(&gpu.composite_pipeline);
                pass.set_bind_group(0, &mask.bind, &[]);
                pass.draw(0..3, 0..1);
            }
        }
        resources.insert(world);
        vec![]
    }
    fn paint(
        &self,
        info: egui::PaintCallbackInfo,
        pass: &mut wgpu::RenderPass<'static>,
        resources: &CallbackResources,
    ) {
        Layers {
            meshes: vec![],
            opacity: 1.,
        }
        .paint(info, pass, resources);
    }
}
#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;

    #[test]
    fn copper_color_geometry_reuses_cache_and_restores_layer_colors_when_disabled() {
        let board = Arc::new(
            Pcb::parse(
                r#"(kicad_pcb (layers (0 "F.Cu" signal)) (net 1 "SIG")
            (segment (start 0 0) (end 5 0) (width 0.5) (layer "F.Cu") (net 1))
            (footprint "R" (layer "F.Cu") (at 10 0) (property "Reference" "R1")
                (pad "1" smd rect (at 0 0) (size 2 2) (layers "F.Cu") (net 1 "SIG")))
            (zone (net 1) (net_name "SIG") (layer "F.Cu")
                (filled_polygon (layer "F.Cu") (pts (xy 20 0) (xy 22 0) (xy 22 2) (xy 20 2)))))"#,
            )
            .unwrap(),
        );
        let ctx = egui::Context::default();
        let mut cache = None;
        let colors = BTreeMap::from([("SIG".into(), [38, 198, 218])]);
        assert!(
            board
                .items
                .iter()
                .filter(|item| super::super::pcb::is_net_copper(item))
                .count()
                >= 3
        );
        let _ = ctx.run(Default::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let colored = colored_scene(
                    ui,
                    &mut cache,
                    &board,
                    "F.Cu",
                    &BTreeSet::new(),
                    1.,
                    &colors,
                );
                assert!(
                    colored
                        .meshes
                        .iter()
                        .flatten()
                        .flat_map(|m| &m.vertices)
                        .any(|v| v.color == Color32::from_rgb(38, 198, 218))
                );
                assert!(
                    colored
                        .meshes
                        .iter()
                        .flatten()
                        .flat_map(|m| &m.vertices)
                        .any(|v| v.pos.x > 9.
                            && v.pos.x < 12.
                            && v.color == Color32::from_rgb(38, 198, 218)),
                    "pad copper uses its net-class color"
                );
                assert!(
                    colored
                        .meshes
                        .iter()
                        .flatten()
                        .flat_map(|m| &m.vertices)
                        .any(|v| v.pos.x >= 20.
                            && v.color == Color32::from_rgb(38, 198, 218).gamma_multiply(0.4)),
                    "filled copper regions retain zone opacity with the class color"
                );
                let same = colored_scene(
                    ui,
                    &mut cache,
                    &board,
                    "F.Cu",
                    &BTreeSet::new(),
                    1.,
                    &colors,
                );
                assert!(Arc::ptr_eq(&colored, &same));
                let normal = cached_scene(ui, &mut cache, &board, "F.Cu", &BTreeSet::new(), 1.);
                assert!(!Arc::ptr_eq(&colored, &normal));
                assert!(
                    !normal
                        .meshes
                        .iter()
                        .flatten()
                        .flat_map(|m| &m.vertices)
                        .any(|v| v.color == Color32::from_rgb(38, 198, 218))
                );
            });
        });
    }
    #[test]
    fn scene_cache_reuses_geometry_and_rebuilds_for_layer_changes() {
        let board=Arc::new(Pcb::parse(r#"(kicad_pcb (layers (0 "F.Cu" signal)) (segment (start 1 1) (end 5 1) (width 0.2) (layer "F.Cu")))"#).unwrap());
        let ctx = egui::Context::default();
        let mut cache = None;
        let _ = ctx.run(Default::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let hidden = BTreeSet::new();
                let a = cached_scene(ui, &mut cache, &board, "F.Cu", &hidden, 0.2);
                let b = cached_scene(ui, &mut cache, &board, "F.Cu", &hidden, 0.2);
                assert!(Arc::ptr_eq(&a, &b));
                let c = cached_scene(ui, &mut cache, &board, "F.Cu", &hidden, 0.3);
                assert!(!Arc::ptr_eq(&a, &c));
                let meshes = geometry_meshes(
                    ui,
                    vec![egui::Shape::circle_filled(
                        egui::pos2(0., 0.),
                        4.,
                        Color32::RED,
                    )],
                    egui::Rect::EVERYTHING,
                    1.,
                );
                assert!(
                    meshes.iter().map(|m| m.vertices.len()).sum::<usize>() > 4,
                    "circles must be geometry, not atlas-image quads"
                );
            });
        });
    }
    #[test]
    #[ignore = "Requires a working GPU adapter"]
    fn overlapping_shapes_in_one_layer_do_not_accumulate_opacity() {
        crate::async_io::runtime().unwrap().block_on(async {
            let instance = wgpu::Instance::default();
            let adapter = instance
                .request_adapter(&wgpu::RequestAdapterOptions::default())
                .await
                .unwrap();
            let (device, queue) = adapter
                .request_device(&wgpu::DeviceDescriptor::default())
                .await
                .unwrap();
            let mut resources = CallbackResources::default();
            resources.insert(Gpu::new(&device, wgpu::TextureFormat::Rgba8Unorm));
            let mut mesh = egui::Mesh::default();
            for (left, right) in [(4., 36.), (20., 52.)] {
                mesh.add_colored_rect(
                    egui::Rect::from_min_max(egui::pos2(left, 4.), egui::pos2(right, 12.)),
                    Color32::RED,
                );
            }
            let layers = Layers {
                meshes: vec![vec![mesh]],
                opacity: 0.2,
            };
            let mut encoder = device.create_command_encoder(&Default::default());
            layers.prepare(
                &device,
                &queue,
                &ScreenDescriptor {
                    size_in_pixels: [64, 16],
                    pixels_per_point: 1.,
                },
                &mut encoder,
                &mut resources,
            );
            let buffer = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("PCB test readback"),
                size: 256 * 16,
                usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                mapped_at_creation: false,
            });
            let gpu = resources.get::<Gpu>().unwrap();
            encoder.copy_texture_to_buffer(
                gpu.output.as_ref().unwrap()._texture.as_image_copy(),
                wgpu::TexelCopyBufferInfo {
                    buffer: &buffer,
                    layout: wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(256),
                        rows_per_image: Some(16),
                    },
                },
                wgpu::Extent3d {
                    width: 64,
                    height: 16,
                    depth_or_array_layers: 1,
                },
            );
            queue.submit([encoder.finish()]);
            let (tx, rx) = std::sync::mpsc::channel();
            buffer
                .slice(..)
                .map_async(wgpu::MapMode::Read, move |r| tx.send(r).unwrap());
            device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
            rx.recv().unwrap().unwrap();
            let bytes = buffer.slice(..).get_mapped_range();
            let pixel = |x: usize| bytes[8 * 256 + x * 4..8 * 256 + x * 4 + 4].to_vec();
            assert_eq!(pixel(8), pixel(24));
            assert_eq!(pixel(24), pixel(44));
            assert_eq!(pixel(24), vec![51, 0, 0, 51]);
        });
    }
    #[test]
    #[ignore = "Requires a working GPU adapter"]
    fn camera_moves_geometry_without_reuploading_scene() {
        crate::async_io::runtime().unwrap().block_on(async {
            let instance = wgpu::Instance::default();
            let adapter = instance
                .request_adapter(&wgpu::RequestAdapterOptions::default())
                .await
                .unwrap();
            let (device, queue) = adapter
                .request_device(&wgpu::DeviceDescriptor::default())
                .await
                .unwrap();
            let mut resources = CallbackResources::default();
            resources.insert(Gpu::new(&device, wgpu::TextureFormat::Rgba8Unorm));
            let mut mesh = egui::Mesh::default();
            mesh.add_colored_rect(
                egui::Rect::from_min_max(egui::pos2(4., 4.), egui::pos2(12., 12.)),
                Color32::RED,
            );
            let scene = Arc::new(WorldScene {
                meshes: vec![vec![mesh]],
            });
            let buffer = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("Camera test readback"),
                size: 256 * 16,
                usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                mapped_at_creation: false,
            });
            for (center, expected) in [(32., vec![255, 0, 0, 255]), (40., vec![0, 0, 0, 0])] {
                let camera = WorldLayers {
                    scene: scene.clone(),
                    rect: egui::Rect::from_min_max(egui::pos2(0., 0.), egui::pos2(64., 16.)),
                    center: Point::new(center, 8.),
                    scale: 1.,
                };
                let mut encoder = device.create_command_encoder(&Default::default());
                camera.prepare(
                    &device,
                    &queue,
                    &ScreenDescriptor {
                        size_in_pixels: [64, 16],
                        pixels_per_point: 1.,
                    },
                    &mut encoder,
                    &mut resources,
                );
                let gpu = resources.get::<Gpu>().unwrap();
                encoder.copy_texture_to_buffer(
                    gpu.output.as_ref().unwrap()._texture.as_image_copy(),
                    wgpu::TexelCopyBufferInfo {
                        buffer: &buffer,
                        layout: wgpu::TexelCopyBufferLayout {
                            offset: 0,
                            bytes_per_row: Some(256),
                            rows_per_image: Some(16),
                        },
                    },
                    wgpu::Extent3d {
                        width: 64,
                        height: 16,
                        depth_or_array_layers: 1,
                    },
                );
                queue.submit([encoder.finish()]);
                let (tx, rx) = std::sync::mpsc::channel();
                buffer
                    .slice(..)
                    .map_async(wgpu::MapMode::Read, move |r| tx.send(r).unwrap());
                device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
                rx.recv().unwrap().unwrap();
                {
                    let bytes = buffer.slice(..).get_mapped_range();
                    assert_eq!(&bytes[8 * 256 + 8 * 4..8 * 256 + 8 * 4 + 4], expected);
                }
                buffer.unmap();
                assert_eq!(resources.get::<WorldGpu>().unwrap().uploads, 1);
            }
        });
    }
}
