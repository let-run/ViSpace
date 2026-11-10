//! Retained depth-tested GPU rendering. Interaction changes camera uniforms, not geometry.
use crate::{Scene, Selection, Vertex};
use egui_wgpu::{CallbackResources, CallbackTrait, ScreenDescriptor, wgpu};
use glam::{Mat4, Vec3};
use std::hash::{Hash, Hasher};
use std::sync::{Arc, Mutex};
use wgpu::util::DeviceExt;
#[derive(Clone, Debug)]
pub struct Camera {
    pub center: [f32; 3],
    pub distance: f32,
    pub yaw: f32,
    pub pitch: f32,
}
impl Camera {
    pub fn fit(scene: &Scene) -> Self {
        Self {
            center: scene.center,
            distance: scene.radius * 2.8,
            yaw: -1.1,
            pitch: 0.95,
        }
    }
    pub fn matrix(&self, aspect: f32) -> Mat4 {
        let center = Vec3::from_array(self.center);
        let dir = Vec3::new(
            self.yaw.cos() * self.pitch.cos(),
            self.yaw.sin() * self.pitch.cos(),
            self.pitch.sin(),
        );
        // Reversed floating-point depth preserves separation between micron-scale
        // board coatings without sacrificing the full orbit/zoom range.
        Mat4::perspective_infinite_reverse_rh(
            45_f32.to_radians(),
            aspect.max(0.01),
            (self.distance * 0.01).max(0.001),
        ) * Mat4::look_at_rh(center + dir * self.distance, center, Vec3::Z)
    }
}
struct MeshGpu {
    vertices: wgpu::Buffer,
    indices: wgpu::Buffer,
    count: u32,
}
struct InstanceGpu {
    uniform: wgpu::Buffer,
    bind: wgpu::BindGroup,
    mesh: usize,
    transform: Mat4,
    reference: String,
}
struct Loaded {
    id: usize,
    meshes: Vec<MeshGpu>,
    instances: Vec<InstanceGpu>,
}
struct Target {
    _color: wgpu::Texture,
    view: wgpu::TextureView,
    _depth: wgpu::Texture,
    depth: wgpu::TextureView,
    bind: wgpu::BindGroup,
    size: [u32; 2],
}
type FrameKey = ([f32; 16], [u32; 2], usize, u32, u64, u32);
pub type PickRay = ([f32; 3], [f32; 3]);
struct Renderer {
    pipeline: wgpu::RenderPipeline,
    blit: wgpu::RenderPipeline,
    uniform_layout: wgpu::BindGroupLayout,
    texture_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    format: wgpu::TextureFormat,
    loaded: Option<Loaded>,
    target: Option<Target>,
    last: Option<FrameKey>,
}
#[derive(Clone)]
struct Shared(Arc<Mutex<Renderer>>, Arc<Mutex<Renderer>>);
fn shared_renderer(device: &wgpu::Device, format: wgpu::TextureFormat) -> Shared {
    Shared(
        Arc::new(Mutex::new(renderer(device, format))),
        Arc::new(Mutex::new(renderer(device, format))),
    )
}
const SHADER: &str = r#"
struct Camera {mvp:mat4x4<f32>,normal:mat4x4<f32>,lighting:vec4<f32>}
@group(0) @binding(0) var<uniform> camera:Camera;
struct Out {@builtin(position) p:vec4<f32>,@location(0) normal:vec3<f32>,@location(1) color:vec3<f32>}
@vertex fn vertex(@location(0) p:vec3<f32>,@location(1) n:vec3<f32>,@location(2) color:vec3<f32>)->Out {
 var out:Out;out.p=camera.mvp*vec4<f32>(p,1.);out.normal=(camera.normal*vec4<f32>(n,0.)).xyz;out.color=color;return out;
}
// Model and board palette colours are display RGB. Light them in linear space;
// the sRGB render target performs the single output encoding.
fn linear_color(color:vec3<f32>)->vec3<f32> {
 return select(pow((color+vec3<f32>(0.055))/1.055,vec3<f32>(2.4)),color/12.92,color<=vec3<f32>(0.04045));
}
@fragment fn fragment(in:Out,@builtin(front_facing) front:bool)->@location(0) vec4<f32> {
 let n=normalize(in.normal)*select(-1.,1.,front);
 // Matched upper/lower studio lights keep either board face readable.
 let key=max(0.,dot(n,normalize(vec3<f32>(-0.4,-0.6,1.))));
 let lower=max(0.,dot(n,normalize(vec3<f32>(-0.4,-0.6,-1.))));
 let fill=abs(dot(n,normalize(vec3<f32>(0.7,0.4,0.3))));
 // A small neutral environment reflection keeps truly black plastics readable.
 let exposed=linear_color(in.color)*(0.22+0.68*max(key,lower)+0.08*fill)*camera.lighting.x
     +vec3<f32>(0.005)*(0.5+0.5*max(key,lower));
 // Soft shoulder: retain pale plastic/metal shading rather than clipping to white.
 // Scale RGB together to preserve hue. A soft shadow toe lifts dark materials
 // independently of the highlights, retaining the mask/trace contrast.
 let peak=max(exposed.r,max(exposed.g,exposed.b));
 let shadow=peak+0.23*(1.-exp(-peak/0.025))*(1.-smoothstep(0.12,0.45,peak));
 let excess=max(shadow-0.60,0.);
 let mapped=select(shadow,0.60+excess/(1.+excess/0.40),shadow>0.60);
 let lit=exposed*(mapped/max(peak,0.00001));
 let grey=dot(lit,vec3<f32>(0.2126,0.7152,0.0722));
 let muted=mix(vec3<f32>(0.045,0.055,0.07),vec3<f32>(grey),camera.lighting.z);
 return vec4<f32>(mix(lit,muted,camera.lighting.y),1.);
}
@group(0) @binding(0) var image:texture_2d<f32>;
@group(0) @binding(1) var image_sampler:sampler;
struct Full {@builtin(position) p:vec4<f32>,@location(0) uv:vec2<f32>}
@vertex fn full(@builtin(vertex_index) i:u32)->Full {
 let p=array<vec2<f32>,3>(vec2<f32>(-1.,-1.),vec2<f32>(3.,-1.),vec2<f32>(-1.,3.));
 var out:Full;out.p=vec4<f32>(p[i],0.,1.);out.uv=vec2<f32>((p[i].x+1.)*0.5,(1.-p[i].y)*0.5);return out;
}
@fragment fn blit(in:Full)->@location(0) vec4<f32> {return textureSample(image,image_sampler,in.uv);}
"#;
// Separate modules avoid overlapping bind group definitions between independent entry points.
fn renderer(device: &wgpu::Device, format: wgpu::TextureFormat) -> Renderer {
    let (geometry_source, blit_source) = SHADER
        .split_once("@group(0) @binding(0) var image")
        .unwrap();
    let geometry = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("3D board lighting"),
        source: wgpu::ShaderSource::Wgsl(geometry_source.into()),
    });
    let blit_source = format!("@group(0) @binding(0) var image{blit_source}");
    let blit_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("3D board composite"),
        source: wgpu::ShaderSource::Wgsl(blit_source.into()),
    });
    let uniform_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: None,
        entries: &[wgpu::BindGroupLayoutEntry {
            binding: 0,
            visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        }],
    });
    let texture_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: None,
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 1,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                count: None,
            },
        ],
    });
    let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: None,
        bind_group_layouts: &[&uniform_layout],
        push_constant_ranges: &[],
    });
    let attributes = wgpu::vertex_attr_array![0=>Float32x3,1=>Float32x3,2=>Float32x3];
    let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("3D board depth pass"),
        layout: Some(&layout),
        vertex: wgpu::VertexState {
            module: &geometry,
            entry_point: Some("vertex"),
            compilation_options: Default::default(),
            buffers: &[wgpu::VertexBufferLayout {
                array_stride: std::mem::size_of::<Vertex>() as u64,
                step_mode: wgpu::VertexStepMode::Vertex,
                attributes: &attributes,
            }],
        },
        fragment: Some(wgpu::FragmentState {
            module: &geometry,
            entry_point: Some("fragment"),
            compilation_options: Default::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format,
                blend: None,
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        primitive: wgpu::PrimitiveState {
            cull_mode: None,
            ..Default::default()
        },
        depth_stencil: Some(wgpu::DepthStencilState {
            format: wgpu::TextureFormat::Depth32Float,
            depth_write_enabled: true,
            depth_compare: wgpu::CompareFunction::GreaterEqual,
            stencil: Default::default(),
            bias: Default::default(),
        }),
        multisample: Default::default(),
        multiview: None,
        cache: None,
    });
    let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: None,
        bind_group_layouts: &[&texture_layout],
        push_constant_ranges: &[],
    });
    let blit = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("3D board final frame"),
        layout: Some(&layout),
        vertex: wgpu::VertexState {
            module: &blit_shader,
            entry_point: Some("full"),
            compilation_options: Default::default(),
            buffers: &[],
        },
        fragment: Some(wgpu::FragmentState {
            module: &blit_shader,
            entry_point: Some("blit"),
            compilation_options: Default::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format,
                blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        primitive: Default::default(),
        depth_stencil: None,
        multisample: Default::default(),
        multiview: None,
        cache: None,
    });
    let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
        mag_filter: wgpu::FilterMode::Linear,
        min_filter: wgpu::FilterMode::Linear,
        ..Default::default()
    });
    Renderer {
        pipeline,
        blit,
        uniform_layout,
        texture_layout,
        sampler,
        format,
        loaded: None,
        target: None,
        last: None,
    }
}
pub fn register(state: &egui_wgpu::RenderState, ctx: &egui::Context) {
    let shared = shared_renderer(&state.device, state.target_format);
    ctx.data_mut(|data| data.insert_temp(egui::Id::new("kicad_3d_gpu"), shared.clone()));
    state.renderer.write().callback_resources.insert(shared);
}
pub fn release(ctx: &egui::Context, scene: &Scene) {
    if let Some(shared) = ctx.data(|data| data.get_temp::<Shared>(egui::Id::new("kicad_3d_gpu"))) {
        for renderer in [&shared.0, &shared.1] {
            let mut gpu = renderer.lock().unwrap();
            if gpu
                .loaded
                .as_ref()
                .is_some_and(|loaded| loaded.id == scene.id)
            {
                gpu.loaded = None;
                gpu.target = None;
                gpu.last = None;
            }
        }
    }
}
struct Frame {
    scene: Arc<Scene>,
    matrix: [f32; 16],
    brightness: f32,
    selection: Selection,
    context: f32,
    overlay: bool,
    size: egui::Vec2,
}
impl CallbackTrait for Frame {
    fn prepare(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        screen: &ScreenDescriptor,
        encoder: &mut wgpu::CommandEncoder,
        resources: &mut CallbackResources,
    ) -> Vec<wgpu::CommandBuffer> {
        let Some(shared) = resources.get::<Shared>() else {
            return vec![];
        };
        let mut renderer = if self.overlay {
            shared.1.lock().unwrap()
        } else {
            shared.0.lock().unwrap()
        };
        let r = &mut *renderer;
        let limit = device.limits().max_texture_dimension_2d;
        let size = [
            (self.size.x * screen.pixels_per_point)
                .round()
                .clamp(1., limit as f32) as u32,
            (self.size.y * screen.pixels_per_point)
                .round()
                .clamp(1., limit as f32) as u32,
        ];
        if r.loaded.as_ref().is_none_or(|s| s.id != self.scene.id) {
            let meshes = self
                .scene
                .meshes
                .iter()
                .map(|mesh| MeshGpu {
                    vertices: device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                        label: Some("3D model vertices"),
                        contents: bytemuck::cast_slice(&mesh.vertices),
                        usage: wgpu::BufferUsages::VERTEX,
                    }),
                    indices: device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                        label: Some("3D model triangles"),
                        contents: bytemuck::cast_slice(&mesh.indices),
                        usage: wgpu::BufferUsages::INDEX,
                    }),
                    count: mesh.indices.len() as u32,
                })
                .collect();
            let instances = self
                .scene
                .instances
                .iter()
                .map(|instance| {
                    let uniform = device.create_buffer(&wgpu::BufferDescriptor {
                        label: Some("3D camera/model transform"),
                        size: 144,
                        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                        mapped_at_creation: false,
                    });
                    let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
                        label: None,
                        layout: &r.uniform_layout,
                        entries: &[wgpu::BindGroupEntry {
                            binding: 0,
                            resource: uniform.as_entire_binding(),
                        }],
                    });
                    InstanceGpu {
                        uniform,
                        bind,
                        mesh: instance.mesh,
                        transform: Mat4::from_cols_array(&instance.transform),
                        reference: instance.reference.clone(),
                    }
                })
                .collect();
            r.loaded = Some(Loaded {
                id: self.scene.id,
                meshes,
                instances,
            });
            r.last = None;
        }
        if r.target.as_ref().is_none_or(|t| t.size != size) {
            let descriptor = wgpu::TextureDescriptor {
                label: Some("3D finished-frame cache"),
                size: wgpu::Extent3d {
                    width: size[0],
                    height: size[1],
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: r.format,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                    | wgpu::TextureUsages::TEXTURE_BINDING
                    | wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            };
            let color = device.create_texture(&descriptor);
            let view = color.create_view(&Default::default());
            let depth = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("3D depth buffer"),
                format: wgpu::TextureFormat::Depth32Float,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                ..descriptor
            });
            let depth_view = depth.create_view(&Default::default());
            let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: None,
                layout: &r.texture_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(&view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::Sampler(&r.sampler),
                    },
                ],
            });
            r.target = Some(Target {
                _color: color,
                view,
                _depth: depth,
                depth: depth_view,
                bind,
                size,
            });
            r.last = None;
        }
        let mut hash = std::collections::hash_map::DefaultHasher::new();
        self.selection.hash(&mut hash);
        let key = (
            self.matrix,
            size,
            self.scene.id,
            self.brightness.to_bits(),
            hash.finish(),
            self.context.to_bits(),
        );
        if r.last.as_ref() == Some(&key) {
            return vec![];
        }
        let loaded = r.loaded.as_ref().unwrap();
        let target = r.target.as_ref().unwrap();
        let camera = Mat4::from_cols_array(&self.matrix);
        for instance in &loaded.instances {
            let mut data = [0.; 36];
            data[..16].copy_from_slice(&(camera * instance.transform).to_cols_array());
            data[16..32].copy_from_slice(&instance.transform.inverse().transpose().to_cols_array());
            data[32] = self.brightness;
            data[33] = if !self.overlay
                && !self.selection.is_empty()
                && !self.selection.references.contains(&instance.reference)
            {
                1.
            } else {
                0.
            };
            data[34] = self.context;
            queue.write_buffer(&instance.uniform, 0, bytemuck::cast_slice(&data));
        }
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("3D board"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &target.view,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(if self.overlay {
                            wgpu::Color::TRANSPARENT
                        } else {
                            wgpu::Color {
                                r: 0.045,
                                g: 0.055,
                                b: 0.07,
                                a: 1.,
                            }
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &target.depth,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(0.),
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                ..Default::default()
            });
            pass.set_pipeline(&r.pipeline);
            for instance in &loaded.instances {
                let mesh = &loaded.meshes[instance.mesh];
                pass.set_bind_group(0, &instance.bind, &[]);
                pass.set_vertex_buffer(0, mesh.vertices.slice(..));
                pass.set_index_buffer(mesh.indices.slice(..), wgpu::IndexFormat::Uint32);
                pass.draw_indexed(0..mesh.count, 0, 0..1);
            }
        }
        r.last = Some(key);
        vec![]
    }
    fn paint(
        &self,
        _: egui::PaintCallbackInfo,
        pass: &mut wgpu::RenderPass<'static>,
        resources: &CallbackResources,
    ) {
        if let Some(shared) = resources.get::<Shared>() {
            let r = if self.overlay {
                shared.1.lock().unwrap()
            } else {
                shared.0.lock().unwrap()
            };
            if let Some(target) = &r.target {
                pass.set_pipeline(&r.blit);
                pass.set_bind_group(0, &target.bind, &[]);
                pass.draw(0..3, 0..1);
            }
        }
    }
}
pub fn draw(ui: &mut egui::Ui, scene: &Arc<Scene>, camera: &mut Camera) -> egui::Rect {
    draw_with_brightness(ui, scene, camera, 1.2)
}
/// Adjust lighting using uniforms only; retained meshes do not need rebuilding.
pub fn draw_with_brightness(
    ui: &mut egui::Ui,
    scene: &Arc<Scene>,
    camera: &mut Camera,
    brightness: f32,
) -> egui::Rect {
    draw_selected(
        ui,
        scene,
        camera,
        brightness,
        &Selection::default(),
        None,
        0.4,
    )
    .0
}
#[allow(clippy::too_many_arguments)]
pub fn draw_selected(
    ui: &mut egui::Ui,
    scene: &Arc<Scene>,
    camera: &mut Camera,
    brightness: f32,
    selection: &Selection,
    overlay: Option<&Arc<Scene>>,
    context: f32,
) -> (egui::Rect, Option<PickRay>) {
    let (rect, response) = ui.allocate_exact_size(
        ui.available_size().max(egui::vec2(1., 1.)),
        egui::Sense::click_and_drag(),
    );
    if response.double_clicked() {
        *camera = Camera::fit(scene);
    }
    if response.dragged() {
        let delta = ui.input(|i| i.pointer.delta());
        if response.dragged_by(egui::PointerButton::Secondary) || ui.input(|i| i.modifiers.shift) {
            let right = Vec3::new(-camera.yaw.sin(), camera.yaw.cos(), 0.);
            let up = Vec3::new(
                -camera.yaw.cos() * camera.pitch.sin(),
                -camera.yaw.sin() * camera.pitch.sin(),
                camera.pitch.cos(),
            );
            camera.center = (Vec3::from_array(camera.center)
                + (up * delta.y - right * delta.x) * camera.distance / rect.height())
            .to_array();
        } else {
            camera.yaw -= delta.x * 0.008;
            camera.pitch = (camera.pitch + delta.y * 0.008).clamp(-1.56, 1.56);
        }
        ui.ctx().request_repaint();
    }
    if response.hovered() {
        let factor = ui.input(|i| (-i.smooth_scroll_delta.y * 0.002).exp() / i.zoom_delta());
        camera.distance =
            (camera.distance * factor).clamp(scene.radius * 0.03, scene.radius * 100.);
    }
    if ui
        .ctx()
        .data(|data| data.get_temp::<Shared>(egui::Id::new("kicad_3d_gpu")))
        .is_some()
    {
        ui.painter().add(egui_wgpu::Callback::new_paint_callback(
            rect,
            Frame {
                scene: scene.clone(),
                matrix: camera.matrix(rect.aspect_ratio()).to_cols_array(),
                brightness: brightness.clamp(0.5, 3.),
                selection: selection.clone(),
                context,
                overlay: false,
                size: rect.size(),
            },
        ));
    } else {
        ui.painter().text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            "3D rendering requires the wgpu backend",
            egui::FontId::proportional(14.),
            egui::Color32::WHITE,
        );
    }
    if let Some(overlay) = overlay {
        ui.painter().add(egui_wgpu::Callback::new_paint_callback(
            rect,
            Frame {
                scene: overlay.clone(),
                matrix: camera.matrix(rect.aspect_ratio()).to_cols_array(),
                brightness: brightness.clamp(0.5, 3.),
                selection: Selection::default(),
                context,
                overlay: true,
                size: rect.size(),
            },
        ));
    }
    let ray = if response.clicked() {
        response.interact_pointer_pos().map(|p| {
            let uv = (p - rect.min) / rect.size();
            let inverse = camera.matrix(rect.aspect_ratio()).inverse();
            let near = inverse.project_point3(Vec3::new(uv.x * 2. - 1., 1. - uv.y * 2., 1.));
            let far = inverse.project_point3(Vec3::new(uv.x * 2. - 1., 1. - uv.y * 2., 0.001));
            (near.to_array(), (far - near).normalize().to_array())
        })
    } else {
        None
    };
    (rect, ray)
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;
    fn block_on<F: std::future::Future>(future: F) -> F::Output {
        struct Wake(std::thread::Thread);
        impl std::task::Wake for Wake {
            fn wake(self: Arc<Self>) {
                self.0.unpark();
            }
            fn wake_by_ref(self: &Arc<Self>) {
                self.0.unpark();
            }
        }
        let waker = std::task::Waker::from(Arc::new(Wake(std::thread::current())));
        let mut context = std::task::Context::from_waker(&waker);
        let mut future = Box::pin(future);
        loop {
            match future.as_mut().poll(&mut context) {
                std::task::Poll::Ready(output) => return output,
                std::task::Poll::Pending => std::thread::park(),
            }
        }
    }
    #[test]
    fn thin_surface_layers_remain_depth_distinguishable_during_orbit_and_zoom() {
        for distance in [2., 10., 100., 1000., 10_000.] {
            for yaw in [-2., -0.2, 1., 2.5] {
                for pitch in [-1.55_f32, -0.7, 0.7, 1.55] {
                    let camera = Camera {
                        center: [150., 130., 0.],
                        distance,
                        yaw,
                        pitch,
                    };
                    let matrix = camera.matrix(1.5);
                    let side = pitch.signum();
                    let depth = |height| matrix.project_point3(Vec3::new(150., 130., height)).z;
                    let substrate = depth(side * 0.8);
                    let copper = depth(side * 0.804);
                    let via = depth(side * 0.81);
                    let pad = depth(side * 0.815);
                    assert!(substrate > 0. && pad < 1.);
                    assert!(
                        copper > substrate && via > copper && pad > via,
                        "surface separation lost at distance={distance}, yaw={yaw}, pitch={pitch}: {substrate}, {copper}, {via}, {pad}"
                    );
                }
            }
        }
    }
    #[test]
    #[ignore = "Requires a working GPU adapter; optionally set KICAD_3D_SCENE and KICAD_3D_PREVIEW"]
    fn depth_render_camera_reuses_buffers_and_finished_frame() {
        block_on(async {
            let instance = wgpu::Instance::default();
            let adapter = instance.request_adapter(&Default::default()).await.unwrap();
            let (device, queue) = adapter
                .request_device(&wgpu::DeviceDescriptor {
                    label: Some("3D renderer test"),
                    ..Default::default()
                })
                .await
                .unwrap();
            let scene = if let Ok(path) = std::env::var("KICAD_3D_SCENE") {
                Arc::new(
                    bincode::serde::decode_from_std_read(
                        &mut std::fs::File::open(path).unwrap(),
                        bincode::config::standard(),
                    )
                    .unwrap(),
                )
            } else {
                crate::build(
                    &crate::scene::tests::board(),
                    &std::collections::BTreeMap::new(),
                    || false,
                )
                .unwrap()
            };
            let shared = shared_renderer(&device, wgpu::TextureFormat::Rgba8UnormSrgb);
            let mut resources = CallbackResources::default();
            resources.insert(shared.clone());
            let mut camera = Camera::fit(&scene);
            if let Some(zoom) = std::env::var("KICAD_3D_ZOOM")
                .ok()
                .and_then(|s| s.parse::<f32>().ok())
            {
                camera.distance /= zoom.clamp(0.1, 10.);
            }
            if std::env::var_os("KICAD_3D_BOTTOM").is_some() {
                camera.pitch = -1.56;
            }
            let size = egui::vec2(1000., 750.);
            let render_mode = |camera: &Camera,
                               resources: &mut CallbackResources,
                               brightness: f32,
                               selected: Selection,
                               overlay: bool,
                               shown: Arc<Scene>| {
                let mut encoder = device.create_command_encoder(&Default::default());
                Frame {
                    scene: shown,
                    matrix: camera.matrix(size.x / size.y).to_cols_array(),
                    brightness,
                    selection: selected,
                    context: 0.4,
                    overlay,
                    size,
                }
                .prepare(
                    &device,
                    &queue,
                    &ScreenDescriptor {
                        size_in_pixels: [1000, 750],
                        pixels_per_point: 1.,
                    },
                    &mut encoder,
                    resources,
                );
                queue.submit([encoder.finish()]);
                device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
            };
            let render = |camera: &Camera, resources: &mut CallbackResources, brightness: f32| {
                render_mode(
                    camera,
                    resources,
                    brightness,
                    Selection::default(),
                    false,
                    scene.clone(),
                )
            };
            render(&camera, &mut resources, 1.6);
            let original = shared.0.lock().unwrap().loaded.as_ref().unwrap().meshes[0]
                .vertices
                .clone();
            let read_mode = |overlay: bool| {
                let row = 4096;
                let buffer = device.create_buffer(&wgpu::BufferDescriptor {
                    label: None,
                    size: row * 750,
                    usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                    mapped_at_creation: false,
                });
                let mut encoder = device.create_command_encoder(&Default::default());
                encoder.copy_texture_to_buffer(
                    (if overlay { &shared.1 } else { &shared.0 })
                        .lock()
                        .unwrap()
                        .target
                        .as_ref()
                        .unwrap()
                        ._color
                        .as_image_copy(),
                    wgpu::TexelCopyBufferInfo {
                        buffer: &buffer,
                        layout: wgpu::TexelCopyBufferLayout {
                            offset: 0,
                            bytes_per_row: Some(row as u32),
                            rows_per_image: Some(750),
                        },
                    },
                    wgpu::Extent3d {
                        width: 1000,
                        height: 750,
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
                let mapped = buffer.slice(..).get_mapped_range();
                let pixels = mapped
                    .chunks_exact(row as usize)
                    .flat_map(|r| r[..4000].iter().copied())
                    .collect::<Vec<_>>();
                drop(mapped);
                buffer.unmap();
                pixels
            };
            let read = || read_mode(false);
            let before = read();
            let background = before[..4].to_vec();
            let non_background = before
                .chunks_exact(4)
                .filter(|p| *p != background.as_slice())
                .count();
            assert!(
                non_background > 10_000,
                "3D board must occupy the rendered frame"
            );
            render(&camera, &mut resources, 1.6);
            assert_eq!(before, read(), "unchanged frame is reused exactly");
            if let Ok(path) = std::env::var("KICAD_3D_PREVIEW") {
                let file = std::fs::File::create(path).unwrap();
                let mut encoder = png::Encoder::new(file, 1000, 750);
                encoder.set_color(png::ColorType::Rgba);
                encoder.set_depth(png::BitDepth::Eight);
                encoder
                    .write_header()
                    .unwrap()
                    .write_image_data(&before)
                    .unwrap();
            }
            if std::env::var_os("KICAD_3D_SCENE").is_none() {
                // Use the plain fixture to compare the same green substrate on either face.
                let green_brightness = |pixels: &[u8]| {
                    let green: Vec<_> = pixels
                        .chunks_exact(4)
                        .filter(|p| {
                            p[1] as f32 > p[0] as f32 * 1.4 && p[1] as f32 > p[2] as f32 * 1.1
                        })
                        .map(|p| p[1] as f32)
                        .collect();
                    assert!(green.len() > 1000);
                    green.iter().sum::<f32>() / green.len() as f32
                };
                camera.pitch = -camera.pitch;
                render(&camera, &mut resources, 1.6);
                let underside = read();
                let ratio = green_brightness(&underside) / green_brightness(&before);
                assert!(
                    (0.85..1.15).contains(&ratio),
                    "board lighting should match on both sides: {ratio}"
                );
                camera.pitch = -camera.pitch;
            }
            render(&camera, &mut resources, 0.6);
            let dim = read();
            let light_sum = |pixels: &[u8]| {
                pixels
                    .chunks_exact(4)
                    .filter(|p| *p != background.as_slice())
                    .map(|p| p[0] as u64 + p[1] as u64 + p[2] as u64)
                    .sum::<u64>()
            };
            assert!(
                light_sum(&before) > light_sum(&dim),
                "brightness slider must invalidate the cached frame"
            );
            if let Ok(path) = std::env::var("KICAD_3D_SELECTION_PREVIEW") {
                let selected = Selection {
                    nets: std::collections::BTreeSet::new(),
                    net: std::env::var("KICAD_3D_NET").ok(),
                    references: std::env::var("KICAD_3D_REFERENCE")
                        .ok()
                        .into_iter()
                        .collect(),
                };
                render_mode(
                    &camera,
                    &mut resources,
                    1.6,
                    selected.clone(),
                    false,
                    scene.clone(),
                );
                let mut pixels = read();
                if selected.net.is_some() {
                    let board = inventory_kicad::Pcb::parse(
                        &std::fs::read_to_string(std::env::var("KICAD_3D_BOARD").unwrap()).unwrap(),
                    )
                    .unwrap();
                    let overlay = crate::build_selection(&board, &selected, || false).unwrap();
                    assert!(!overlay.meshes.is_empty(), "selection overlay is empty");
                    render_mode(
                        &camera,
                        &mut resources,
                        1.6,
                        Selection::default(),
                        true,
                        overlay,
                    );
                    let top = read_mode(true);
                    for (base, over) in pixels.chunks_exact_mut(4).zip(top.chunks_exact(4)) {
                        if over[3] > 0 {
                            base.copy_from_slice(over);
                        }
                    }
                }
                let mut png = png::Encoder::new(std::fs::File::create(path).unwrap(), 1000, 750);
                png.set_color(png::ColorType::Rgba);
                png.set_depth(png::BitDepth::Eight);
                png.write_header()
                    .unwrap()
                    .write_image_data(&pixels)
                    .unwrap();
            }
            let selection = Selection {
                nets: std::collections::BTreeSet::new(),
                net: Some("SIG".into()),
                references: Default::default(),
            };
            render_mode(
                &camera,
                &mut resources,
                1.6,
                selection.clone(),
                false,
                scene.clone(),
            );
            let grey = read();
            assert_ne!(before, grey, "selection must invalidate the base frame");
            let board = crate::scene::tests::board();
            let mut board = board;
            board.items.push(inventory_kicad::PcbItem {
                reference: String::new(),
                net: "SIG".into(),
                zone: false,
                layers: vec!["F.Cu".into()],
                shape: inventory_kicad::PcbShape::Path {
                    points: vec![
                        inventory_kicad::Point::new(
                            scene.center[0] as f64 - 4.,
                            -scene.center[1] as f64 - 4.,
                        ),
                        inventory_kicad::Point::new(
                            scene.center[0] as f64 + 4.,
                            -scene.center[1] as f64 + 4.,
                        ),
                    ],
                    width: 0.6,
                    filled: false,
                    triangles: vec![],
                },
            });
            let overlay = crate::build_selection(&board, &selection, || false).unwrap();
            render_mode(
                &camera,
                &mut resources,
                1.6,
                Selection::default(),
                true,
                overlay,
            );
            let highlighted = read_mode(true);
            assert!(
                highlighted.chunks_exact(4).filter(|p| p[3] > 0).count() > 100,
                "overlay must draw coloured net geometry"
            );
            assert!(
                highlighted.chunks_exact(4).filter(|p| p[3] == 0).count() > 100_000,
                "overlay background must stay transparent"
            );
            assert_eq!(
                grey,
                read(),
                "overlay must retain the base target and buffers"
            );
            camera.yaw += 0.8;
            camera.distance *= 0.85;
            render(&camera, &mut resources, 1.6);
            assert_ne!(before, read(), "orbit/zoom changes the image");
            assert_eq!(
                original,
                shared.0.lock().unwrap().loaded.as_ref().unwrap().meshes[0].vertices,
                "camera changes retain GPU geometry"
            );
            // Pale STEP plastics and connector metals must retain both their
            // material differences and normal-based shading at high exposure.
            let mut geometry = crate::scene::Geometry::default();
            for (i, (color, normal)) in [
                ([0.8; 3], [0., 0., 1.]),
                ([0.97, 0.969, 0.93], [0., 0., 1.]),
                ([0.97, 0.969, 0.93], [0.8, 0., 0.6]),
            ]
            .into_iter()
            .enumerate()
            {
                let x = i as f32 * 2. - 3.;
                let base = geometry.vertices.len() as u32;
                for position in [
                    [x, -1., 0.],
                    [x + 1.8, -1., 0.],
                    [x + 1.8, 1., 0.],
                    [x, 1., 0.],
                ] {
                    geometry.vertices.push(crate::scene::Vertex {
                        position,
                        normal,
                        color,
                    });
                }
                geometry
                    .indices
                    .extend([base, base + 1, base + 2, base, base + 2, base + 3]);
            }
            let pale = Arc::new(Scene {
                id: usize::MAX,
                meshes: vec![geometry],
                instances: vec![crate::scene::Instance {
                    mesh: 0,
                    transform: Mat4::IDENTITY.to_cols_array(),
                    reference: "pale".into(),
                }],
                center: [0.; 3],
                radius: 4.,
                statistics: Default::default(),
                warnings: vec![],
            });
            let mut pale_camera = Camera::fit(&pale);
            pale_camera.pitch = 1.56;
            for brightness in [1.2, 3.] {
                render_mode(
                    &pale_camera,
                    &mut resources,
                    brightness,
                    Selection::default(),
                    false,
                    pale.clone(),
                );
                let pixels = read();
                let surface: Vec<_> = pixels
                    .chunks_exact(4)
                    .filter(|p| p[0] > 100 && p[1] > 100 && p[2] > 100)
                    .collect();
                assert!(surface.len() > 1000);
                assert!(
                    surface.iter().all(|p| p[..3].iter().all(|c| *c < 254)),
                    "pale surfaces clipped at brightness {brightness}"
                );
                let levels: std::collections::BTreeSet<_> =
                    surface.iter().map(|p| (p[0], p[1], p[2])).collect();
                if brightness == 1.2 {
                    let darkest = surface.iter().map(|p| p[0]).min().unwrap();
                    let lightest = surface.iter().map(|p| p[0]).max().unwrap();
                    assert!(
                        lightest - darkest >= 15,
                        "pale materials need visible shading contrast: {darkest}..{lightest}"
                    );
                    assert!(
                        lightest < 240,
                        "default lighting still washes out pale models: {lightest}"
                    );
                }
                assert!(
                    levels.len() >= 3,
                    "material and normal shading collapsed at brightness {brightness}"
                );
            }
            // Check dark solder mask, masked copper, and black plastic together:
            // dark details must be visible without raising the white highlights.
            let mut geometry = pale.meshes[0].clone();
            for (panel, color) in [[0.035, 0.21, 0.10], [0.065, 0.29, 0.145], [0.; 3]]
                .into_iter()
                .enumerate()
            {
                for v in &mut geometry.vertices[panel * 4..panel * 4 + 4] {
                    v.color = color;
                    v.normal = [0., 0., 1.];
                }
            }
            let dark = Arc::new(Scene {
                id: usize::MAX - 1,
                meshes: vec![geometry],
                instances: pale.instances.clone(),
                center: pale.center,
                radius: pale.radius,
                statistics: Default::default(),
                warnings: vec![],
            });
            render_mode(
                &pale_camera,
                &mut resources,
                1.2,
                Selection::default(),
                false,
                dark,
            );
            let pixels = read();
            let mut greens: Vec<_> = pixels
                .chunks_exact(4)
                .filter(|p| p[1] > p[0].saturating_add(20) && p[1] > p[2].saturating_add(10))
                .map(|p| p[1])
                .collect();
            greens.sort_unstable();
            assert!(greens.len() > 1000);
            let mask = greens[greens.len() / 4];
            let trace = greens[greens.len() * 3 / 4];
            assert!(mask >= 110, "solder mask too dark: {mask}");
            assert!(
                trace >= mask + 15,
                "traces need visible contrast: {mask} vs {trace}"
            );
            assert!(
                pixels
                    .chunks_exact(4)
                    .filter(|p| p[0] >= 40 && p[0] <= 90 && p[0] == p[1] && p[1] == p[2])
                    .count()
                    > 1000,
                "black plastic lost in shadows"
            );
        });
    }
}
