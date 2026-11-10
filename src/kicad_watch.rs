//! Portable polling watcher: filesystem I/O runs on Tokio, never the UI/Rayon pool.
//! Two stable samples debounce editor writes, including temporary-file/rename saves.
use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::Duration,
};
#[derive(Clone, Debug, PartialEq, Eq)]
struct Stamp {
    len: u64,
    modified: Option<std::time::SystemTime>,
}
async fn stamps(paths: &[PathBuf]) -> Vec<Option<Stamp>> {
    let mut output = Vec::with_capacity(paths.len());
    for path in paths {
        output.push(tokio::fs::metadata(path).await.ok().map(|m| Stamp {
            len: m.len(),
            modified: m.modified().ok(),
        }));
    }
    output
}
pub struct Watch {
    receiver: mpsc::Receiver<()>,
    cancel: Arc<AtomicBool>,
    active: Arc<AtomicBool>,
}
impl Watch {
    pub fn new(paths: Vec<PathBuf>, wake: impl Fn() + Send + 'static) -> anyhow::Result<Self> {
        let (sender, receiver) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let token = cancel.clone();
        let active = Arc::new(AtomicBool::new(true));
        let enabled = active.clone();
        crate::async_io::spawn(async move {
            let mut last = stamps(&paths).await;
            let mut candidate = last.clone();
            while !token.load(Ordering::Relaxed) {
                tokio::time::sleep(Duration::from_millis(500)).await;
                if token.load(Ordering::Relaxed) {
                    break;
                }
                if !enabled.load(Ordering::Relaxed) {
                    continue;
                }
                let current = stamps(&paths).await;
                if current != last && current == candidate {
                    last = current.clone();
                    if sender.send(()).is_err() {
                        break;
                    }
                    wake();
                }
                candidate = current;
            }
        })?;
        Ok(Self {
            receiver,
            cancel,
            active,
        })
    }
    /// Pausing preserves the last observed stamps, so resuming detects intervening edits.
    pub fn set_active(&self, active: bool) {
        self.active.store(active, Ordering::Relaxed);
    }
    pub fn changed(&self) -> bool {
        let mut changed = false;
        while self.receiver.try_recv().is_ok() {
            changed = true;
        }
        changed
    }
}
impl Drop for Watch {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}
