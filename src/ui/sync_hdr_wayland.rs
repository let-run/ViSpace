//! Present the Sync icon as PQ content on a non-interactive Wayland subsurface.
use raw_window_handle::{HasDisplayHandle, HasWindowHandle, RawDisplayHandle, RawWindowHandle};
use std::{
    io::{Seek, Write},
    os::fd::AsFd,
};
use wayland_client::{
    Connection, Dispatch, EventQueue, Proxy, QueueHandle, WEnum, delegate_noop,
    protocol::{
        wl_buffer, wl_compositor, wl_region, wl_registry, wl_shm, wl_shm_pool, wl_subcompositor,
        wl_subsurface, wl_surface,
    },
};
use wayland_protocols::wp::color_management::v1::client::{
    wp_color_management_surface_feedback_v1 as feedback,
    wp_color_management_surface_v1 as color_surface, wp_color_manager_v1 as color,
    wp_image_description_creator_params_v1 as creator, wp_image_description_info_v1 as info,
    wp_image_description_v1 as description,
};

pub(super) const ICON_ID: &str = "sync-hdr-icon";
pub(super) type Icon = (egui::Rect, egui::Color32);

#[derive(Default)]
pub(super) struct Overlay {
    session: Option<Session>,
    unavailable: bool,
}

struct Session {
    queue: EventQueue<State>,
    connection: Connection,
    parent: wl_surface::WlSurface,
    state: State,
}

#[derive(Default)]
struct State {
    compositor: Option<wl_compositor::WlCompositor>,
    subcompositor: Option<wl_subcompositor::WlSubcompositor>,
    shm: Option<wl_shm::WlShm>,
    manager: Option<color::WpColorManagerV1>,
    feedback: Option<feedback::WpColorManagementSurfaceFeedbackV1>,
    caps: Capabilities,
    hdr: Option<description::WpImageDescriptionV1>,
    hdr_ready: bool,
    preferred_generation: u64,
    luminance: Luminance,
    peak: Option<f32>,
    surface: Option<Surface>,
}

#[derive(Default)]
struct Capabilities {
    parametric: bool,
    pq: bool,
    bt2020: bool,
    perceptual: bool,
    done: bool,
}

#[derive(Default)]
struct Luminance {
    max: u32,
    reference: u32,
    target: Option<u32>,
}

struct Surface {
    surface: wl_surface::WlSurface,
    subsurface: wl_subsurface::WlSubsurface,
    color: color_surface::WpColorManagementSurfaceV1,
    buffers: Vec<Buffer>,
    visible: bool,
    position: Option<(i32, i32)>,
}

struct Buffer {
    proxy: wl_buffer::WlBuffer,
    file: std::fs::File,
    size: i32,
    ready: bool,
}

impl Drop for Buffer {
    fn drop(&mut self) {
        self.proxy.destroy();
    }
}

impl Drop for Surface {
    fn drop(&mut self) {
        self.color.destroy();
        self.subsurface.destroy();
        self.surface.destroy();
    }
}

#[derive(Clone, Copy)]
enum Description {
    Content,
    Preferred(u64),
}

impl Session {
    fn new(frame: &eframe::Frame) -> Option<Self> {
        let display = frame.display_handle().ok()?;
        let window = frame.window_handle().ok()?;
        let (RawDisplayHandle::Wayland(display), RawWindowHandle::Wayland(window)) =
            (display.as_raw(), window.as_raw())
        else {
            return None;
        };
        // Winit owns the display and surface. This guest queue neither reads from nor
        // closes the connection. The app drops it during on_exit, before Winit exits.
        let backend = unsafe {
            wayland_backend::client::Backend::from_foreign_display(display.display.as_ptr().cast())
        };
        let connection = Connection::from_backend(backend);
        let parent = unsafe {
            wayland_backend::client::ObjectId::from_ptr(
                wl_surface::WlSurface::interface(),
                window.surface.as_ptr().cast(),
            )
            .ok()?
        };
        let parent = wl_surface::WlSurface::from_id(&connection, parent).ok()?;
        let queue = connection.new_event_queue();
        connection.display().get_registry(&queue.handle(), ());
        Some(Self {
            queue,
            connection,
            parent,
            state: State::default(),
        })
    }

    fn draw(&mut self, ctx: &egui::Context, icon: Option<Icon>) -> anyhow::Result<()> {
        self.queue.dispatch_pending(&mut self.state)?;
        let state = &mut self.state;
        let qh = self.queue.handle();
        if state.caps.done
            && state.caps.parametric
            && state.caps.pq
            && state.caps.bt2020
            && state.caps.perceptual
            && let Some(manager) = &state.manager
            && state.hdr.is_none()
        {
            let creator = manager.create_parametric_creator(&qh, ());
            creator.set_tf_named(color::TransferFunction::St2084Pq);
            creator.set_primaries_named(color::Primaries::Bt2020);
            state.hdr = Some(creator.create(&qh, Description::Content));
            let feedback = manager.get_surface_feedback(&self.parent, &qh, ());
            state.request_preferred(&feedback, &qh);
            state.feedback = Some(feedback);
        }
        if let (Some((rect, tint)), Some(peak), true) = (icon, state.peak, state.hdr_ready) {
            if state.surface.is_none()
                && let (Some(compositor), Some(subcompositor), Some(manager), Some(hdr)) = (
                    &state.compositor,
                    &state.subcompositor,
                    &state.manager,
                    &state.hdr,
                )
            {
                let surface = compositor.create_surface(&qh, ());
                // An empty input region leaves the egui Sync button clickable.
                let empty = compositor.create_region(&qh, ());
                surface.set_input_region(Some(&empty));
                empty.destroy();
                let subsurface = subcompositor.get_subsurface(&surface, &self.parent, &qh, ());
                subsurface.set_desync();
                let color = manager.get_surface(&surface, &qh, ());
                color.set_image_description(hdr, color::RenderIntent::Perceptual);
                state.surface = Some(Surface {
                    surface,
                    subsurface,
                    color,
                    buffers: Vec::with_capacity(2),
                    visible: false,
                    position: None,
                });
            }
            if let (Some(surface), Some(shm)) = (&mut state.surface, &state.shm) {
                surface.draw(shm, &qh, ctx, rect, tint, peak)?;
            }
        } else if let Some(surface) = &mut state.surface
            && surface.visible
        {
            surface.surface.attach(None, 0, 0);
            surface.surface.commit();
            surface.visible = false;
        }
        match self.connection.flush() {
            Ok(()) => Ok(()),
            Err(wayland_client::backend::WaylandError::Io(error))
                if error.kind() == std::io::ErrorKind::WouldBlock =>
            {
                Ok(())
            }
            Err(error) => Err(error.into()),
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        drop(self.state.surface.take());
        if let Some(feedback) = self.state.feedback.take() {
            feedback.destroy();
        }
        if let Some(description) = self.state.hdr.take() {
            description.destroy();
        }
        if let Some(manager) = self.state.manager.take() {
            manager.destroy();
        }
        let _ = self.connection.flush();
    }
}

impl Overlay {
    pub(super) fn update(
        &mut self,
        frame: &eframe::Frame,
        ctx: &egui::Context,
        icon: Option<Icon>,
    ) {
        if self.unavailable {
            return;
        }
        if self.session.is_none() {
            if icon.is_none() {
                return;
            }
            self.session = Session::new(frame);
            self.unavailable = self.session.is_none();
        }
        if let Some(session) = &mut self.session
            && session.draw(ctx, icon).is_err()
        {
            self.session = None;
            self.unavailable = true;
        }
    }
}

impl Surface {
    fn draw(
        &mut self,
        shm: &wl_shm::WlShm,
        qh: &QueueHandle<State>,
        ctx: &egui::Context,
        rect: egui::Rect,
        tint: egui::Color32,
        peak: f32,
    ) -> anyhow::Result<()> {
        let native_scale = ctx
            .input(|input| input.viewport().native_pixels_per_point)
            .unwrap_or(1.);
        let zoom = ctx.pixels_per_point() / native_scale;
        let rect = rect.expand(3.);
        let logical_size = (rect.width() * zoom).round().clamp(1., 128.) as i32;
        let scale = if self.surface.version() >= 3 {
            native_scale.ceil().clamp(1., 4.) as i32
        } else {
            1
        };
        let size = logical_size * scale;
        self.buffers
            .retain(|buffer| buffer.size == size || !buffer.ready);
        let index = if let Some(index) = self.buffers.iter().position(|buffer| buffer.ready) {
            index
        } else if self.buffers.len() < 2 {
            let file = tempfile::tempfile()?;
            let bytes = size * size * 4;
            file.set_len(bytes as u64)?;
            let pool = shm.create_pool(file.as_fd(), bytes, qh, ());
            let proxy =
                pool.create_buffer(0, size, size, size * 4, wl_shm::Format::Argb8888, qh, ());
            pool.destroy();
            self.buffers.push(Buffer {
                proxy,
                file,
                size,
                ready: true,
            });
            self.buffers.len() - 1
        } else {
            return Ok(());
        };
        let buffer = &mut self.buffers[index];
        buffer.file.rewind()?;
        buffer.file.write_all(&icon_pixels(size, tint, peak))?;
        let position = (
            (rect.left() * zoom).round() as i32,
            (rect.top() * zoom).round() as i32,
        );
        if self.position != Some(position) {
            self.subsurface.set_position(position.0, position.1);
            self.position = Some(position);
        }
        if self.surface.version() >= 3 {
            self.surface.set_buffer_scale(scale);
        }
        self.surface.attach(Some(&buffer.proxy), 0, 0);
        self.surface.damage(0, 0, logical_size, logical_size);
        self.surface.commit();
        buffer.ready = false;
        self.visible = true;
        Ok(())
    }
}

fn icon_pixels(size: i32, tint: egui::Color32, peak: f32) -> Vec<u8> {
    let [r, g, b, _] = egui::Rgba::from(tint).to_array();
    let intensity = peak / r.max(g).max(b);
    // Linear sRGB to linear BT.2020, then absolute ST 2084 (PQ) encoding.
    let rgb = [
        pq(intensity * (0.6274 * r + 0.3293 * g + 0.0433 * b)),
        pq(intensity * (0.0691 * r + 0.9195 * g + 0.0114 * b)),
        pq(intensity * (0.0164 * r + 0.0880 * g + 0.8956 * b)),
    ];
    let nodes = [
        egui::vec2(0.5, 0.19),
        egui::vec2(0.18, 0.76),
        egui::vec2(0.82, 0.76),
    ];
    let mut bytes = Vec::with_capacity((size * size * 4) as usize);
    for y in 0..size {
        for x in 0..size {
            let point = (egui::vec2(x as f32 + 0.5, y as f32 + 0.5) * (20. / size as f32)
                - egui::Vec2::splat(3.))
                / 14.;
            let mut distance = 1_f32;
            for (i, node) in nodes.into_iter().enumerate() {
                distance = distance.min(((point - node).length() - 0.12).abs());
                let next = nodes[(i + 1) % nodes.len()];
                let inset = (next - node).normalized() * 0.12;
                let start = node + inset;
                let link = next - inset - start;
                let t = ((point - start).dot(link) / link.length_sq()).clamp(0., 1.);
                distance = distance.min((point - start - link * t).length());
            }
            let core = ((0.07 - distance) / 0.035).clamp(0., 1.);
            let halo = 0.12 * ((0.19 - distance) / 0.135).clamp(0., 1.);
            let alpha = core.max(halo);
            let encode = |value: f32| (value * alpha * 255.).round() as u32;
            let argb =
                encode(1.) << 24 | encode(rgb[0]) << 16 | encode(rgb[1]) << 8 | encode(rgb[2]);
            bytes.extend(argb.to_le_bytes());
        }
    }
    bytes
}

fn pq(nits: f32) -> f32 {
    let luminance = (nits / 10_000.).clamp(0., 1.).powf(0.15930176);
    ((0.8359375 + 18.851563 * luminance) / (1. + 18.6875 * luminance)).powf(78.84375)
}

impl State {
    fn request_preferred(
        &mut self,
        feedback: &feedback::WpColorManagementSurfaceFeedbackV1,
        qh: &QueueHandle<Self>,
    ) {
        self.preferred_generation = self.preferred_generation.wrapping_add(1);
        self.luminance = Luminance::default();
        self.peak = None;
        feedback.get_preferred(qh, Description::Preferred(self.preferred_generation));
    }
}

impl Dispatch<wl_registry::WlRegistry, ()> for State {
    fn event(
        state: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        let wl_registry::Event::Global {
            name,
            interface,
            version,
        } = event
        else {
            return;
        };
        match interface.as_str() {
            "wl_compositor" => state.compositor = Some(registry.bind(name, version.min(3), qh, ())),
            "wl_subcompositor" => state.subcompositor = Some(registry.bind(name, 1, qh, ())),
            "wl_shm" => state.shm = Some(registry.bind(name, 1, qh, ())),
            "wp_color_manager_v1" => state.manager = Some(registry.bind(name, 1, qh, ())),
            _ => {}
        }
    }
}

impl Dispatch<color::WpColorManagerV1, ()> for State {
    fn event(
        state: &mut Self,
        _: &color::WpColorManagerV1,
        event: color::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            color::Event::SupportedFeature {
                feature: WEnum::Value(color::Feature::Parametric),
            } => state.caps.parametric = true,
            color::Event::SupportedTfNamed {
                tf: WEnum::Value(color::TransferFunction::St2084Pq),
            } => state.caps.pq = true,
            color::Event::SupportedPrimariesNamed {
                primaries: WEnum::Value(color::Primaries::Bt2020),
            } => state.caps.bt2020 = true,
            color::Event::SupportedIntent {
                render_intent: WEnum::Value(color::RenderIntent::Perceptual),
            } => state.caps.perceptual = true,
            color::Event::Done => state.caps.done = true,
            _ => {}
        }
    }
}

impl Dispatch<feedback::WpColorManagementSurfaceFeedbackV1, ()> for State {
    fn event(
        state: &mut Self,
        feedback: &feedback::WpColorManagementSurfaceFeedbackV1,
        _: feedback::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        state.request_preferred(feedback, qh);
    }
}

impl Dispatch<description::WpImageDescriptionV1, Description> for State {
    fn event(
        state: &mut Self,
        description: &description::WpImageDescriptionV1,
        event: description::Event,
        kind: &Description,
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        let ready = matches!(event, description::Event::Ready { .. });
        match kind {
            Description::Content => state.hdr_ready = ready,
            Description::Preferred(generation) => {
                if ready && *generation == state.preferred_generation {
                    description.get_information(qh, *generation);
                }
                description.destroy();
            }
        }
    }
}

impl Dispatch<info::WpImageDescriptionInfoV1, u64> for State {
    fn event(
        state: &mut Self,
        _: &info::WpImageDescriptionInfoV1,
        event: info::Event,
        generation: &u64,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if *generation != state.preferred_generation {
            return;
        }
        match event {
            info::Event::Luminances {
                max_lum,
                reference_lum,
                ..
            } => {
                state.luminance.max = max_lum;
                state.luminance.reference = reference_lum;
            }
            info::Event::TargetLuminance { max_lum, .. } => state.luminance.target = Some(max_lum),
            info::Event::Done => {
                let peak = state.luminance.target.unwrap_or(state.luminance.max);
                state.peak = (state.luminance.reference > 0 && peak > state.luminance.reference)
                    .then_some(peak.min(10_000) as f32);
            }
            _ => {}
        }
    }
}

impl Dispatch<wl_buffer::WlBuffer, ()> for State {
    fn event(
        state: &mut Self,
        proxy: &wl_buffer::WlBuffer,
        _: wl_buffer::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let Some(surface) = &mut state.surface
            && let Some(buffer) = surface
                .buffers
                .iter_mut()
                .find(|buffer| buffer.proxy == *proxy)
        {
            buffer.ready = true;
        }
    }
}

delegate_noop!(State: ignore wl_compositor::WlCompositor);
delegate_noop!(State: ignore wl_region::WlRegion);
delegate_noop!(State: ignore wl_subcompositor::WlSubcompositor);
delegate_noop!(State: ignore wl_subsurface::WlSubsurface);
delegate_noop!(State: ignore wl_surface::WlSurface);
delegate_noop!(State: ignore wl_shm::WlShm);
delegate_noop!(State: ignore wl_shm_pool::WlShmPool);
delegate_noop!(State: ignore color_surface::WpColorManagementSurfaceV1);
delegate_noop!(State: ignore creator::WpImageDescriptionCreatorParamsV1);
