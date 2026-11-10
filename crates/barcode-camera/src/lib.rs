//! Camera adapters share the same bounded frame slot and portable Rust decoder.
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Device {
    pub id: String,
    pub name: String,
}
#[derive(Clone)]
pub struct Frame {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
    pub sequence: u64,
}
#[derive(Clone, Default)]
pub struct Snapshot {
    pub devices: Vec<Device>,
    pub selected: String,
    pub frame: Option<Frame>,
    pub code: Option<String>,
    pub error: Option<String>,
}
struct Shared {
    state: Mutex<Snapshot>,
    stopped: AtomicBool,
    wake: Arc<dyn Fn() + Send + Sync>,
}
impl Shared {
    fn error(&self, message: impl ToString) {
        self.state.lock().unwrap().error = Some(message.to_string());
        (self.wake)();
    }
}
pub struct Session {
    shared: Arc<Shared>,
}
impl Session {
    pub fn start(preferred: String, wake: impl Fn() + Send + Sync + 'static) -> Self {
        let shared = Arc::new(Shared {
            state: Mutex::new(Snapshot {
                selected: preferred,
                ..Default::default()
            }),
            stopped: AtomicBool::new(false),
            wake: Arc::new(wake),
        });
        platform::start(shared.clone());
        Self { shared }
    }
    pub fn snapshot(&self) -> Snapshot {
        let mut state = self.shared.state.lock().unwrap();
        let frame = state.frame.take();
        let mut snapshot = state.clone();
        snapshot.frame = frame;
        snapshot
    }
    pub fn select(&self, id: String) {
        let mut state = self.shared.state.lock().unwrap();
        if state.devices.iter().any(|d| d.id == id) && state.selected != id {
            state.selected = id;
            state.frame = None;
            state.error = None;
        }
    }
}
impl Drop for Session {
    fn drop(&mut self) {
        self.shared.stopped.store(true, Ordering::Relaxed);
    }
}
/// Invalid/blank frames are ordinary non-matches, never decoder panics.
pub fn decode_rgba(rgba: &[u8], width: u32, height: u32) -> Option<String> {
    let pixels = (width as usize).checked_mul(height as usize)?;
    if pixels == 0 || pixels > 4096 * 4096 || rgba.len() != pixels.checked_mul(4)? {
        return None;
    }
    let luma: Vec<u8> = rgba
        .chunks_exact(4)
        .map(|p| ((u32::from(p[0]) * 77 + u32::from(p[1]) * 150 + u32::from(p[2]) * 29) >> 8) as u8)
        .collect();
    rxing::helpers::detect_in_luma_slice(&luma, width, height, None)
        .ok()
        .map(|r| r.getText().to_owned())
        .filter(|s| !s.trim().is_empty())
}
pub fn resolve_device(preferred: &str, devices: &[Device]) -> Option<String> {
    devices
        .iter()
        .find(|d| d.id == preferred)
        .or_else(|| devices.first())
        .map(|d| d.id.clone())
}
#[cfg(not(target_arch = "wasm32"))]
#[path = "native.rs"]
mod platform;
#[cfg(target_arch = "wasm32")]
#[path = "browser.rs"]
mod platform;

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn closing_session_cancels_and_preview_is_consumed_once() {
        let shared = Arc::new(Shared {
            state: Mutex::new(Snapshot {
                frame: Some(Frame {
                    width: 1,
                    height: 1,
                    rgba: vec![255; 4],
                    sequence: 1,
                }),
                ..Default::default()
            }),
            stopped: AtomicBool::new(false),
            wake: Arc::new(|| {}),
        });
        let session = Session {
            shared: shared.clone(),
        };
        assert!(session.snapshot().frame.is_some());
        assert!(session.snapshot().frame.is_none());
        drop(session);
        assert!(shared.stopped.load(Ordering::Relaxed));
    }
    #[test]
    fn qr_label_round_trip() {
        let text = "8DA80669-86EE-4DFC-A98F-D1FB2B6B97F7";
        let code = qrcode::QrCode::new(text).unwrap();
        let side = code.width() + 8;
        let scale = 5;
        let mut rgba = vec![255; side * side * scale * scale * 4];
        for y in 0..code.width() {
            for x in 0..code.width() {
                if code[(x, y)] == qrcode::Color::Dark {
                    for dy in 0..scale {
                        for dx in 0..scale {
                            let offset =
                                (((y + 4) * scale + dy) * side * scale + (x + 4) * scale + dx) * 4;
                            rgba[offset..offset + 3].fill(0);
                        }
                    }
                }
            }
        }
        assert_eq!(
            decode_rgba(&rgba, (side * scale) as u32, (side * scale) as u32).as_deref(),
            Some(text)
        );
    }
    #[test]
    fn code128_round_trip() {
        let text = "C210262";
        use rxing::oned::OneDimensionalCodeWriter;
        let bars = rxing::oned::Code128Writer.encode_oned(text).unwrap();
        let width = (bars.len() + 40) * 3;
        let height = 120;
        let mut rgba = vec![255; width * height * 4];
        for (x, black) in bars.iter().enumerate() {
            if *black {
                for y in 10..110 {
                    for dx in 0..3 {
                        let offset = (y * width + (x + 20) * 3 + dx) * 4;
                        rgba[offset..offset + 3].fill(0);
                    }
                }
            }
        }
        assert_eq!(
            decode_rgba(&rgba, width as u32, height as u32).as_deref(),
            Some(text)
        );
    }
    #[test]
    fn invalid_frames_and_preference() {
        assert!(decode_rgba(&[], 0, 0).is_none());
        assert!(decode_rgba(&[0; 4], 20, 20).is_none());
        assert!(decode_rgba(&vec![255; 64 * 64 * 4], 64, 64).is_none());
        let devices = vec![
            Device {
                id: "a".into(),
                name: "A".into(),
            },
            Device {
                id: "b".into(),
                name: "B".into(),
            },
        ];
        assert_eq!(resolve_device("b", &devices).as_deref(), Some("b"));
        assert_eq!(resolve_device("missing", &devices).as_deref(), Some("a"));
        assert_eq!(resolve_device("", &[]), None);
    }
}
