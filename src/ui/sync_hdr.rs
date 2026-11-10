//! A small EDR overlay leaves egui's ordinary SDR surface and hit testing intact.
use core_graphics::{
    color_space::{CGColorSpace, kCGColorSpaceExtendedLinearSRGB},
    geometry::CGSize,
};
use metal::foreign_types::ForeignType;
use objc2::{
    class, msg_send,
    runtime::{AnyObject, Bool},
};
use objc2_foundation::{NSPoint, NSRect, NSSize};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};

pub(super) const ICON_ID: &str = "sync-hdr-icon";
pub(super) type Icon = (egui::Rect, egui::Color32);

#[derive(Default)]
pub(super) struct Overlay {
    resources: Option<Resources>,
    unavailable: bool,
}

struct Resources {
    layer: metal::MetalLayer,
    queue: metal::CommandQueue,
    pipeline: metal::RenderPipelineState,
    pending: Option<metal::CommandBuffer>,
}

impl Resources {
    fn new() -> Option<Self> {
        let device = metal::Device::system_default()?;
        let library = device
            .new_library_with_source(
                include_str!("sync_hdr.metal"),
                &metal::CompileOptions::new(),
            )
            .ok()?;
        let descriptor = metal::RenderPipelineDescriptor::new();
        let vertex = library.get_function("icon_vertex", None).ok()?;
        let fragment = library.get_function("icon_fragment", None).ok()?;
        descriptor.set_vertex_function(Some(&vertex));
        descriptor.set_fragment_function(Some(&fragment));
        descriptor
            .color_attachments()
            .object_at(0)?
            .set_pixel_format(metal::MTLPixelFormat::RGBA16Float);
        let pipeline = device.new_render_pipeline_state(&descriptor).ok()?;
        let layer = metal::MetalLayer::new();
        layer.set_device(&device);
        layer.set_pixel_format(metal::MTLPixelFormat::RGBA16Float);
        layer.set_opaque(false);
        layer.set_wants_extended_dynamic_range_content(true);
        let colorspace =
            CGColorSpace::create_with_name(unsafe { kCGColorSpaceExtendedLinearSRGB })?;
        // Core Animation retains the color space.
        unsafe {
            let object = layer.as_ptr().cast::<AnyObject>();
            let native_colorspace = colorspace
                .as_ptr()
                .cast::<objc2_core_graphics::CGColorSpace>()
                .as_ref();
            let _: () = msg_send![object, setColorspace: native_colorspace];
            let _: () = msg_send![object, setZPosition: 1.0_f64];
            let _: () = msg_send![object, setHidden: Bool::YES];
        }
        Some(Self {
            layer,
            queue: device.new_command_queue(),
            pipeline,
            pending: None,
        })
    }

    fn hide(&self) {
        // Removing the overlay also releases its claim on the display's EDR headroom.
        unsafe {
            let layer = self.layer.as_ptr().cast::<AnyObject>();
            let _: () = msg_send![layer, setHidden: Bool::YES];
            let _: () = msg_send![layer, removeFromSuperlayer];
        }
    }
}

impl Drop for Resources {
    fn drop(&mut self) {
        self.hide();
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
        let shown = icon.is_some_and(|icon| self.present(frame, ctx, icon));
        if !shown && let Some(resources) = &self.resources {
            resources.hide();
        }
    }

    fn present(&mut self, frame: &eframe::Frame, ctx: &egui::Context, (icon, color): Icon) -> bool {
        let Ok(handle) = frame.window_handle() else {
            return false;
        };
        let RawWindowHandle::AppKit(handle) = handle.as_raw() else {
            return false;
        };
        // The frame keeps this AppKit view alive during the UI callback.
        let view = handle.ns_view.as_ptr().cast::<AnyObject>();
        unsafe {
            let window: *mut AnyObject = msg_send![view, window];
            if window.is_null() {
                return false;
            }
            let screen: *mut AnyObject = msg_send![window, screen];
            let occlusion: usize = msg_send![window, occlusionState];
            if screen.is_null() || occlusion & (1 << 1) == 0 {
                return false;
            }
            let potential: f64 = msg_send![
                screen,
                maximumPotentialExtendedDynamicRangeColorComponentValue
            ];
            let parent: *mut AnyObject = msg_send![view, layer];
            if potential <= 1.0 || parent.is_null() {
                return false;
            }
            if self.resources.is_none() {
                self.resources = Resources::new();
            }
            let Some(resources) = self.resources.as_mut() else {
                self.unavailable = true;
                return false;
            };
            let layer = resources.layer.as_ptr().cast::<AnyObject>();
            let bounds: NSRect = msg_send![parent, bounds];
            let flipped: Bool = msg_send![parent, isGeometryFlipped];
            let backing: f64 = msg_send![window, backingScaleFactor];
            let scale = f64::from(ctx.pixels_per_point()) / backing;
            let rect = icon.expand(3.);
            let width = f64::from(rect.width()) * scale;
            let height = f64::from(rect.height()) * scale;
            let y = if flipped.as_bool() {
                f64::from(rect.top()) * scale
            } else {
                bounds.size.height - f64::from(rect.bottom()) * scale
            };
            let position = NSRect::new(
                NSPoint::new(
                    bounds.origin.x + f64::from(rect.left()) * scale,
                    bounds.origin.y + y,
                ),
                NSSize::new(width, height),
            );
            let _: () = msg_send![class!(CATransaction), begin];
            let _: () = msg_send![class!(CATransaction), setDisableActions: Bool::YES];
            let existing_parent: *mut AnyObject = msg_send![layer, superlayer];
            if existing_parent != parent {
                let _: () = msg_send![parent, addSublayer: layer];
            }
            let _: () = msg_send![layer, setFrame: position];
            let _: () = msg_send![class!(CATransaction), commit];

            if resources.pending.as_ref().is_some_and(|command| {
                !matches!(
                    command.status(),
                    metal::MTLCommandBufferStatus::Completed | metal::MTLCommandBufferStatus::Error
                )
            }) {
                return true;
            }
            let drawable_size = CGSize::new((width * backing).round(), (height * backing).round());
            let current_size = resources.layer.drawable_size();
            if current_size.width != drawable_size.width
                || current_size.height != drawable_size.height
            {
                resources.layer.set_contents_scale(backing);
                resources.layer.set_drawable_size(drawable_size);
            }
            let Some(drawable) = resources.layer.next_drawable() else {
                return false;
            };
            let headroom: f64 = msg_send![screen, maximumExtendedDynamicRangeColorComponentValue];
            let [red, green, blue, _] = egui::Rgba::from(color).to_array();
            let peak = (headroom as f32).max(1.) / red.max(green).max(blue);
            let tint = [red * peak, green * peak, blue * peak, 1_f32];
            let pass = metal::RenderPassDescriptor::new();
            let Some(target) = pass.color_attachments().object_at(0) else {
                return false;
            };
            target.set_texture(Some(drawable.texture()));
            target.set_load_action(metal::MTLLoadAction::DontCare);
            target.set_store_action(metal::MTLStoreAction::Store);
            let command = resources.queue.new_command_buffer();
            let encoder = command.new_render_command_encoder(pass);
            encoder.set_render_pipeline_state(&resources.pipeline);
            encoder.set_fragment_bytes(
                0,
                std::mem::size_of_val(&tint) as u64,
                tint.as_ptr().cast(),
            );
            encoder.draw_primitives(metal::MTLPrimitiveType::Triangle, 0, 3);
            encoder.end_encoding();
            command.present_drawable(drawable);
            command.commit();
            resources.pending = Some(command.to_owned());
            let _: () = msg_send![layer, setHidden: Bool::NO];
        }
        true
    }
}
