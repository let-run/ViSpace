use super::*;
use wasm_bindgen::{JsCast, JsValue, closure::Closure};
use wasm_bindgen_futures::JsFuture;
use web_sys::{CanvasRenderingContext2d, HtmlCanvasElement, HtmlVideoElement, MediaStream};

pub(super) fn start(shared: Arc<Shared>) {
    wasm_bindgen_futures::spawn_local(async move {
        if let Err(error) = capture(&shared).await
            && !shared.stopped.load(Ordering::Relaxed)
        {
            shared.error(error);
        }
    });
}
fn js(error: impl Into<JsValue>) -> anyhow::Error {
    let error = error.into();
    anyhow::anyhow!(
        "Camera unavailable: {error:?}. Camera access requires HTTPS or localhost and browser permission."
    )
}
struct Stream(MediaStream);
impl Drop for Stream {
    fn drop(&mut self) {
        for track in self.0.get_tracks().iter() {
            if let Ok(track) = track.dyn_into::<web_sys::MediaStreamTrack>() {
                track.stop();
            }
        }
    }
}
async fn delay() -> anyhow::Result<()> {
    let promise = js_sys::Promise::new(&mut |resolve, _| {
        let callback = Closure::once_into_js(move || {
            let _ = resolve.call0(&JsValue::NULL);
        });
        let _ = web_sys::window()
            .unwrap()
            .set_timeout_with_callback_and_timeout_and_arguments_0(callback.unchecked_ref(), 200);
    });
    JsFuture::from(promise).await.map_err(js)?;
    Ok(())
}
async fn capture(shared: &Arc<Shared>) -> anyhow::Result<()> {
    let window = web_sys::window().ok_or_else(|| anyhow::anyhow!("Browser window unavailable"))?;
    let media = window.navigator().media_devices().map_err(js)?;
    let document = window
        .document()
        .ok_or_else(|| anyhow::anyhow!("Browser document unavailable"))?;
    let mut sequence = 0;
    while !shared.stopped.load(Ordering::Relaxed) {
        let selected = shared.state.lock().unwrap().selected.clone();
        let video_options = js_sys::Object::new();
        if !selected.is_empty() && !shared.state.lock().unwrap().devices.is_empty() {
            let exact = js_sys::Object::new();
            js_sys::Reflect::set(&exact, &"exact".into(), &selected.clone().into()).map_err(js)?;
            js_sys::Reflect::set(&video_options, &"deviceId".into(), &exact).map_err(js)?;
        } else {
            js_sys::Reflect::set(&video_options, &"facingMode".into(), &"environment".into())
                .map_err(js)?;
        }
        js_sys::Reflect::set(&video_options, &"width".into(), &640.into()).map_err(js)?;
        js_sys::Reflect::set(&video_options, &"height".into(), &480.into()).map_err(js)?;
        let constraints = web_sys::MediaStreamConstraints::new();
        constraints.set_video(&video_options);
        constraints.set_audio(&JsValue::FALSE);
        let stream = Stream(
            JsFuture::from(
                media
                    .get_user_media_with_constraints(&constraints)
                    .map_err(js)?,
            )
            .await
            .map_err(js)?
            .dyn_into::<MediaStream>()
            .map_err(js)?,
        );
        if shared.stopped.load(Ordering::Relaxed) {
            return Ok(());
        }
        let devices = JsFuture::from(media.enumerate_devices().map_err(js)?)
            .await
            .map_err(js)?;
        let devices: Vec<Device> = js_sys::Array::from(&devices)
            .iter()
            .filter_map(|d| d.dyn_into::<web_sys::MediaDeviceInfo>().ok())
            .filter(|d| d.kind() == web_sys::MediaDeviceKind::Videoinput)
            .map(|d| Device {
                id: d.device_id(),
                name: d.label(),
            })
            .collect();
        let active = stream
            .0
            .get_video_tracks()
            .get(0)
            .dyn_into::<web_sys::MediaStreamTrack>()
            .map_err(js)?;
        let settings = active.get_settings();
        let actual = settings
            .get_device_id()
            .filter(|id| !id.is_empty())
            .or_else(|| resolve_device(&selected, &devices))
            .ok_or_else(|| anyhow::anyhow!("No camera available"))?;
        {
            let mut state = shared.state.lock().unwrap();
            state.devices = devices;
            // Preserve a selector change made while permission was pending.
            if state.selected == selected {
                state.selected = if selected.is_empty() {
                    actual.clone()
                } else {
                    resolve_device(&selected, &state.devices).unwrap_or_else(|| actual.clone())
                };
            }
        }
        let video: HtmlVideoElement = document
            .create_element("video")
            .map_err(js)?
            .dyn_into()
            .map_err(js)?;
        video.set_muted(true);
        video.set_attribute("playsinline", "").map_err(js)?;
        video.set_src_object(Some(&stream.0));
        JsFuture::from(video.play().map_err(js)?)
            .await
            .map_err(js)?;
        let canvas: HtmlCanvasElement = document
            .create_element("canvas")
            .map_err(js)?
            .dyn_into()
            .map_err(js)?;
        let context: CanvasRenderingContext2d = canvas
            .get_context("2d")
            .map_err(js)?
            .ok_or_else(|| anyhow::anyhow!("Canvas unavailable"))?
            .dyn_into()
            .map_err(js)?;
        while !shared.stopped.load(Ordering::Relaxed)
            && shared.state.lock().unwrap().selected == actual
        {
            delay().await?;
            if shared.stopped.load(Ordering::Relaxed) {
                break;
            }
            let width = video.video_width().min(640);
            let height = if video.video_width() == 0 {
                0
            } else {
                video.video_height() * width / video.video_width()
            };
            if width == 0 || height == 0 {
                continue;
            }
            canvas.set_width(width);
            canvas.set_height(height);
            context
                .draw_image_with_html_video_element_and_dw_and_dh(
                    &video,
                    0.,
                    0.,
                    width as f64,
                    height as f64,
                )
                .map_err(js)?;
            let rgba = context
                .get_image_data(0., 0., width as f64, height as f64)
                .map_err(js)?
                .data()
                .0;
            let code = decode_rgba(&rgba, width, height);
            sequence += 1;
            let mut state = shared.state.lock().unwrap();
            if state.selected != actual {
                break;
            }
            state.frame = Some(Frame {
                width,
                height,
                rgba,
                sequence,
            });
            if let Some(code) = code {
                state.code = Some(code);
                shared.stopped.store(true, Ordering::Relaxed);
            }
            (shared.wake)();
        }
        video.set_src_object(None); // Stream's guard releases every track, including on errors.
    }
    Ok(())
}
