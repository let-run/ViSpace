use super::*;
use nokhwa::{
    Camera,
    pixel_format::RgbFormat,
    utils::{ApiBackend, RequestedFormat, RequestedFormatType},
};
use std::time::{Duration, Instant};

pub(super) fn start(shared: Arc<Shared>) {
    nokhwa::nokhwa_initialize(move |allowed| {
        if shared.stopped.load(Ordering::Relaxed) {
            return;
        }
        if !allowed {
            shared
                .error("Camera permission denied. Allow camera access in system privacy settings.");
            return;
        }
        let worker = shared.clone();
        if let Err(error) = std::thread::Builder::new()
            .name("inventory-camera".into())
            .spawn(move || {
                if let Err(error) = capture(&worker)
                    && !worker.stopped.load(Ordering::Relaxed)
                {
                    worker.error(error);
                }
            })
        {
            shared.error(format!("Cannot start camera: {error}"));
        }
    });
}
fn capture(shared: &Arc<Shared>) -> anyhow::Result<()> {
    let cameras = nokhwa::query(ApiBackend::Auto)?;
    {
        let mut state = shared.state.lock().unwrap();
        state.devices = cameras
            .iter()
            .map(|c| Device {
                id: device_id(c),
                name: c.human_name(),
            })
            .collect();
        state.selected = resolve_device(&state.selected, &state.devices).ok_or_else(|| {
            anyhow::anyhow!("No camera available. Connect a camera and reopen the scanner.")
        })?;
    }
    (shared.wake)();
    let decoding = Arc::new(AtomicBool::new(false));
    let decoder = rayon::ThreadPoolBuilder::new()
        .num_threads(1)
        .thread_name(|_| "inventory-barcode-decoder".into())
        .build()?;
    let mut sequence = 0;
    while !shared.stopped.load(Ordering::Relaxed) {
        let selected = shared.state.lock().unwrap().selected.clone();
        let info = cameras
            .iter()
            .find(|c| device_id(c) == selected)
            .ok_or_else(|| anyhow::anyhow!("Selected camera disconnected"))?;
        // Some cameras advertise minimum rates that their format setter rejects.
        // Begin at a supported maximum rate, then negotiate the modest preview size.
        let mut camera = Camera::new(
            info.index().clone(),
            RequestedFormat::new::<RgbFormat>(RequestedFormatType::HighestFrameRate(30)),
        )
        .or_else(|_| {
            Camera::new(
                info.index().clone(),
                RequestedFormat::new::<RgbFormat>(RequestedFormatType::AbsoluteHighestFrameRate),
            )
        })?;
        let mut formats = camera.compatible_camera_formats()?;
        formats.retain(|f| <RgbFormat as nokhwa::FormatDecoder>::FORMATS.contains(&f.format()));
        formats.sort_by_key(|f| {
            (
                f.width().abs_diff(640) + f.height().abs_diff(480),
                f.frame_rate().abs_diff(30),
            )
        });
        if let Some(format) = formats.first() {
            // Retain the working initial format when a driver rejects this preference.
            let _ = camera.set_camera_requset(RequestedFormat::new::<RgbFormat>(
                RequestedFormatType::Exact(*format),
            ));
        }
        camera.open_stream()?;
        let mut last_decode = Instant::now() - Duration::from_secs(1);
        while !shared.stopped.load(Ordering::Relaxed)
            && shared.state.lock().unwrap().selected == selected
        {
            let buffer = camera.frame()?;
            let rgb = buffer.decode_image::<RgbFormat>()?;
            let width = rgb.width().min(960);
            let height = rgb.height() * width / rgb.width();
            let mut rgba = Vec::with_capacity(width as usize * height as usize * 4);
            for y in 0..height {
                for x in 0..width {
                    let p = rgb
                        .get_pixel(x * rgb.width() / width, y * rgb.height() / height)
                        .0;
                    rgba.extend_from_slice(&[p[0], p[1], p[2], 255]);
                }
            }
            sequence += 1;
            let frame = Frame {
                width,
                height,
                rgba,
                sequence,
            };
            if last_decode.elapsed() >= Duration::from_millis(200)
                && !decoding.swap(true, Ordering::Relaxed)
            {
                last_decode = Instant::now();
                let frame = frame.clone();
                let shared = shared.clone();
                let decoding = decoding.clone();
                let selected = selected.clone();
                decoder.spawn(move || {
                    let code = decode_rgba(&frame.rgba, frame.width, frame.height);
                    if let Some(code) = code {
                        let mut state = shared.state.lock().unwrap();
                        if !shared.stopped.load(Ordering::Relaxed) && state.selected == selected {
                            state.code = Some(code);
                            shared.stopped.store(true, Ordering::Relaxed);
                        }
                    }
                    decoding.store(false, Ordering::Relaxed);
                    (shared.wake)();
                });
            }
            shared.state.lock().unwrap().frame = Some(frame);
            (shared.wake)();
            // Bound preview traffic even when a driver supplies frames without pacing.
            std::thread::sleep(Duration::from_millis(30));
        }
        camera.stop_stream()?;
    }
    Ok(())
}

// The backend provides the stable device identifier in misc; fall back to index
// for drivers that do not expose one.
fn device_id(camera: &nokhwa::utils::CameraInfo) -> String {
    let id = camera.misc();
    if id.is_empty() {
        camera.index().to_string()
    } else {
        format!("{}:{id}", camera.human_name())
    }
}
