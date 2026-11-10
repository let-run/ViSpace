//! Git bundle creation, incoming transfer and application progress for desktop and browser UI.
use std::{
    collections::BTreeMap,
    sync::{Mutex, OnceLock},
};
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Progress {
    pub received: u64,
    pub total: u64,
    pub receiving: u32,
    pub applying: u32,
    pub creating: u32,
}
impl Progress {
    fn add(self, other: Self) -> Self {
        Self {
            received: self.received.saturating_add(other.received),
            total: self.total.saturating_add(other.total),
            receiving: self.receiving.saturating_add(other.receiving),
            applying: self.applying.saturating_add(other.applying),
            creating: self.creating.saturating_add(other.creating),
        }
    }
}
struct Entry {
    value: Progress,
    started: web_time::Instant,
}
#[derive(Default)]
struct State {
    context: Option<egui::Context>,
    transfers: BTreeMap<u64, Entry>,
    next: u64,
}
fn state() -> &'static Mutex<State> {
    static STATE: OnceLock<Mutex<State>> = OnceLock::new();
    STATE.get_or_init(|| Mutex::new(State::default()))
}
pub(crate) fn observe(ctx: &egui::Context) -> Option<Progress> {
    let mut state = state().lock().unwrap();
    state.context = Some(ctx.clone());
    let mut result = Progress::default();
    for entry in state.transfers.values() {
        // Avoid flashing the bar for fast cached/no-op checks, while keeping long work animated.
        let delay = std::time::Duration::from_millis(150);
        if (entry.value.applying > 0 || entry.value.creating > 0) && entry.started.elapsed() < delay
        {
            ctx.request_repaint_after(delay.saturating_sub(entry.started.elapsed()));
            continue;
        }
        result = result.add(entry.value);
    }
    (result.receiving > 0 || result.applying > 0 || result.creating > 0).then_some(result)
}
fn update(id: u64, value: Option<Progress>) {
    let ctx = {
        let mut state = state().lock().unwrap();
        if let Some(value) = value {
            state
                .transfers
                .entry(id)
                .and_modify(|entry| entry.value = value)
                .or_insert_with(|| Entry {
                    value,
                    started: web_time::Instant::now(),
                });
        } else {
            state.transfers.remove(&id);
        }
        state.context.clone()
    };
    if let Some(ctx) = ctx {
        ctx.request_repaint();
    }
}
#[cfg(not(target_arch = "wasm32"))]
pub(crate) struct Receive {
    id: u64,
    last: std::time::Instant,
}
#[cfg(not(target_arch = "wasm32"))]
impl Receive {
    fn start(value: Progress) -> Self {
        let id = {
            let mut state = state().lock().unwrap();
            state.next += 1;
            state.next + 3
        };
        update(id, Some(value));
        Self {
            id,
            last: std::time::Instant::now(),
        }
    }
    pub(crate) fn new(total: u64) -> Self {
        Self::start(Progress {
            total,
            receiving: 1,
            ..Default::default()
        })
    }
    pub(crate) fn set(&mut self, received: u64, total: u64) {
        if received == total || self.last.elapsed() >= std::time::Duration::from_millis(100) {
            update(
                self.id,
                Some(Progress {
                    received: received.min(total),
                    total,
                    receiving: 1,
                    applying: 0,
                    creating: 0,
                }),
            );
            self.last = std::time::Instant::now();
        }
    }
    pub(crate) fn applying(&self) {
        update(
            self.id,
            Some(Progress {
                applying: 1,
                ..Default::default()
            }),
        );
    }
    pub(crate) fn creating() -> Self {
        Self::start(Progress {
            creating: 1,
            ..Default::default()
        })
    }
    pub(crate) fn processing() -> Self {
        Self::start(Progress {
            applying: 1,
            ..Default::default()
        })
    }
}
#[cfg(not(target_arch = "wasm32"))]
impl Drop for Receive {
    fn drop(&mut self) {
        update(self.id, None);
    }
}
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen::prelude::wasm_bindgen]
pub fn report_git_bundle_receive(received: f64, total: f64, count: u32) {
    let value = (count > 0 && received.is_finite() && total.is_finite() && total > 0.).then_some(
        Progress {
            received: received.max(0.).min(total) as u64,
            total: total.max(0.) as u64,
            receiving: count,
            applying: 0,
            creating: 0,
        },
    );
    update(0, value);
}
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen::prelude::wasm_bindgen]
pub fn report_git_bundle_applying(count: u32) {
    update(
        1,
        (count > 0).then_some(Progress {
            applying: count,
            ..Default::default()
        }),
    );
}

#[cfg(target_arch = "wasm32")]
#[wasm_bindgen::prelude::wasm_bindgen]
pub fn report_git_bundle_creating(count: u32) {
    update(
        2,
        (count > 0).then_some(Progress {
            creating: count,
            ..Default::default()
        }),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn receive_transitions_to_applying_and_cleans_up_on_error() {
        let operation_id = std::cell::Cell::new(0);
        let operation = || -> Result<(), ()> {
            let mut progress = Receive::new(100);
            let id = progress.id;
            operation_id.set(id);
            progress.set(100, 100);
            assert_eq!(state().lock().unwrap().transfers[&id].value.receiving, 1);
            progress.applying();
            assert_eq!(
                state().lock().unwrap().transfers[&id].value,
                Progress {
                    applying: 1,
                    ..Default::default()
                }
            );
            Err(())
        };
        assert!(operation().is_err());
        assert!(
            !state()
                .lock()
                .unwrap()
                .transfers
                .contains_key(&operation_id.get())
        );
    }
    #[test]
    fn combines_receiving_and_processing_without_fake_percentages() {
        let a = Progress {
            received: 25,
            total: 100,
            receiving: 1,
            applying: 0,
            creating: 0,
        };
        let b = Progress {
            applying: 1,
            ..Default::default()
        };
        assert_eq!(
            a.add(b),
            Progress {
                received: 25,
                total: 100,
                receiving: 1,
                applying: 1,
                creating: 0
            }
        );
        assert_eq!(b.total, 0);
        let creating = Progress {
            creating: 2,
            ..Default::default()
        };
        assert_eq!(
            a.add(b).add(creating),
            Progress {
                received: 25,
                total: 100,
                receiving: 1,
                applying: 1,
                creating: 2
            }
        );
    }
}
