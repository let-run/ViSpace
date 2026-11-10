//! Runtime boundary: a shared Rayon desktop pool; a browser build supplies a Web Worker
//! transport for the same owned inputs/serializable results (never run work inline).
use anyhow::Result;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc,
};

#[cfg(not(target_arch = "wasm32"))]
fn pool() -> Result<&'static rayon::ThreadPool, String> {
    static POOL: std::sync::OnceLock<Result<rayon::ThreadPool, String>> =
        std::sync::OnceLock::new();
    POOL.get_or_init(|| {
        rayon::ThreadPoolBuilder::new()
            .num_threads(
                std::thread::available_parallelism()
                    .map_or(1, usize::from)
                    .saturating_sub(1)
                    .clamp(1, 4),
            )
            .thread_name(|i| format!("inventory-worker-{i}"))
            .build()
            .map_err(|e| e.to_string())
    })
    .as_ref()
    .map_err(Clone::clone)
}
/// Rayon computation awaited without blocking a Tokio I/O worker.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) async fn compute_async<T: Send + 'static>(
    work: impl FnOnce() -> T + Send + 'static,
) -> Result<T> {
    let (sender, receiver) = tokio::sync::oneshot::channel();
    pool().map_err(anyhow::Error::msg)?.spawn(move || {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(work))
            .map_err(|_| "Background task failed unexpectedly");
        let _ = sender.send(result);
    });
    receiver
        .await
        .map_err(|_| anyhow::anyhow!("Background worker disconnected"))?
        .map_err(anyhow::Error::msg)
}

pub struct Task<T> {
    receiver: mpsc::Receiver<Result<T, String>>,
    cancel: Arc<AtomicBool>,
    #[cfg(target_arch = "wasm32")]
    worker: Option<web_sys::Worker>,
    #[cfg(target_arch = "wasm32")]
    message: Option<wasm_bindgen::closure::Closure<dyn FnMut(web_sys::MessageEvent)>>,
    #[cfg(target_arch = "wasm32")]
    error: Option<wasm_bindgen::closure::Closure<dyn FnMut(web_sys::Event)>>,
}
impl<T: Send + 'static> Task<T> {
    pub fn spawn(
        work: impl FnOnce(&AtomicBool) -> Result<T> + Send + 'static,
        wake: Box<dyn FnOnce() + Send>,
    ) -> Self {
        Self::spawn_shared(move |token| work(&token), wake)
    }
    pub(crate) fn spawn_shared(
        work: impl FnOnce(Arc<AtomicBool>) -> Result<T> + Send + 'static,
        wake: Box<dyn FnOnce() + Send>,
    ) -> Self {
        let (sender, receiver) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        #[cfg(not(target_arch = "wasm32"))]
        {
            let token = cancel.clone();
            let pool = match pool() {
                Ok(pool) => pool,
                Err(error) => {
                    let _ = sender.send(Err(error));
                    wake();
                    return Self {
                        receiver,
                        cancel,
                        #[cfg(target_arch = "wasm32")]
                        worker: None,
                        #[cfg(target_arch = "wasm32")]
                        message: None,
                        #[cfg(target_arch = "wasm32")]
                        error: None,
                    };
                }
            };
            pool.spawn(move || {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| work(token)))
                    .map_err(|_| "Background task failed unexpectedly".to_string())
                    .and_then(|r| r.map_err(|e| e.to_string()));
                let _ = sender.send(result);
                wake();
            });
        }
        #[cfg(target_arch = "wasm32")]
        {
            // Desktop storage and the browser worker host are separate porting work.
            // Fail explicitly instead of freezing the browser's main thread or calling
            // std::thread::spawn, which is unavailable on ordinary wasm32 browsers.
            drop(work);
            let _ = sender.send(Err(
                "This browser build needs a Web Worker sync backend".into()
            ));
            wake();
        }
        Self {
            receiver,
            cancel,
            #[cfg(target_arch = "wasm32")]
            worker: None,
            #[cfg(target_arch = "wasm32")]
            message: None,
            #[cfg(target_arch = "wasm32")]
            error: None,
        }
    }
    pub fn poll(&self) -> Option<Result<T, String>> {
        match self.receiver.try_recv() {
            Ok(result) => Some(result),
            Err(mpsc::TryRecvError::Empty) => None,
            Err(mpsc::TryRecvError::Disconnected) => {
                Some(Err("Background worker disconnected".into()))
            }
        }
    }
}
impl<T> Drop for Task<T> {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
        #[cfg(target_arch = "wasm32")]
        if let Some(worker) = &self.worker {
            worker.set_onmessage(None);
            worker.set_onerror(None);
            worker.terminate();
        }
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;
    use std::{sync::Barrier, time::Duration};
    #[test]
    fn poll_never_waits_for_worker_and_completion_wakes_ui() {
        let gate = Arc::new(Barrier::new(2));
        let worker_gate = gate.clone();
        let (sender, woken) = mpsc::channel();
        let task = Task::spawn(
            move |_| {
                worker_gate.wait();
                assert!(
                    std::thread::current()
                        .name()
                        .unwrap()
                        .starts_with("inventory-worker-")
                );
                Ok(42)
            },
            Box::new(move || {
                sender.send(()).unwrap();
            }),
        );
        assert!(task.poll().is_none());
        gate.wait();
        woken.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(task.poll().unwrap().unwrap(), 42);
    }
    #[test]
    fn dropping_review_signals_cooperative_cancellation() {
        let gate = Arc::new(Barrier::new(2));
        let worker_gate = gate.clone();
        let (sender, observed) = mpsc::channel();
        let task = Task::spawn(
            move |cancel| {
                worker_gate.wait();
                sender.send(cancel.load(Ordering::Relaxed)).unwrap();
                Ok(())
            },
            Box::new(|| {}),
        );
        drop(task);
        gate.wait();
        assert!(observed.recv_timeout(Duration::from_secs(5)).unwrap());
    }
}

#[cfg(target_arch = "wasm32")]
impl<T: Send + 'static> Task<T> {
    pub fn browser(
        input: crate::web::jobs::Input,
        complete: impl FnOnce(serde_json::Value) -> Result<T> + 'static,
        ctx: eframe::egui::Context,
    ) -> Self {
        Self::browser_updates(input, complete, |_| {}, ctx)
    }
    pub fn browser_updates(
        input: crate::web::jobs::Input,
        complete: impl FnOnce(serde_json::Value) -> Result<T> + 'static,
        mut update: impl FnMut(serde_json::Value) + 'static,
        ctx: eframe::egui::Context,
    ) -> Self {
        use wasm_bindgen::{JsCast, closure::Closure};
        let (sender, receiver) = mpsc::channel();
        let mut task = Self {
            receiver,
            cancel: Arc::new(AtomicBool::new(false)),
            worker: None,
            message: None,
            error: None,
        };
        let result = (|| -> Result<()> {
            let options = web_sys::WorkerOptions::new();
            options.set_type(web_sys::WorkerType::Module);
            let worker = web_sys::Worker::new_with_options("worker.js", &options)
                .map_err(crate::web::js_error)?;
            let out = sender.clone();
            let wake = ctx.clone();
            let mut complete = Some(complete);
            let message = Closure::<dyn FnMut(web_sys::MessageEvent)>::new(
                move |event: web_sys::MessageEvent| {
                    if let Some(response) = event
                        .data()
                        .as_string()
                        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
                        && let Some(value) = response.get("update")
                    {
                        update(value.clone());
                        wake.request_repaint();
                        return;
                    }
                    let Some(complete) = complete.take() else {
                        return;
                    };
                    let result = (|| -> Result<T> {
                        let mut response: serde_json::Value = serde_json::from_str(
                            &event
                                .data()
                                .as_string()
                                .ok_or_else(|| anyhow::anyhow!("Invalid worker response"))?,
                        )?;
                        if let Some(error) = response["error"].as_str() {
                            anyhow::bail!(error.to_owned());
                        }
                        complete(
                            response
                                .get_mut("value")
                                .map(std::mem::take)
                                .unwrap_or(serde_json::Value::Null),
                        )
                    })()
                    .map_err(|e| e.to_string());
                    let _ = out.send(result);
                    wake.request_repaint();
                },
            );
            let out = sender.clone();
            let wake = ctx.clone();
            let error = Closure::<dyn FnMut(web_sys::Event)>::new(move |_| {
                let _ = out.send(Err(
                    "Browser worker failed to load. Reload the page and try again.".into(),
                ));
                wake.request_repaint();
            });
            worker.set_onmessage(Some(message.as_ref().unchecked_ref()));
            worker.set_onerror(Some(error.as_ref().unchecked_ref()));
            worker
                .post_message(&serde_json::to_string(&input)?.into())
                .map_err(crate::web::js_error)?;
            task.worker = Some(worker);
            task.message = Some(message);
            task.error = Some(error);
            Ok(())
        })();
        if let Err(e) = result {
            let _ = sender.send(Err(e.to_string()));
            ctx.request_repaint();
        }
        task
    }
}
