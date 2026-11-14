//! Bridge between the egui UI thread and the tokio runtime: spawn a future, poll its result next frame.

use std::future::Future;
use std::sync::OnceLock;

static RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();

pub fn init() {
    RUNTIME.get_or_init(|| tokio::runtime::Builder::new_multi_thread().worker_threads(4).thread_name("bi-net").enable_all().build().expect("tokio runtime"));
}

pub fn handle() -> tokio::runtime::Handle {
    RUNTIME.get().expect("runtime::init not called").handle().clone()
}

/// A value being computed on the runtime. Poll with [`Pending::poll`] from the UI thread.
pub struct Pending<T> {
    rx: Option<tokio::sync::oneshot::Receiver<T>>,
}

/// A job result that can stand in for a task that died without producing one.
pub trait Failable {
    fn task_failed() -> Self;
}

impl<T> Failable for anyhow::Result<T> {
    fn task_failed() -> Self {
        Err(anyhow::anyhow!("the background task stopped unexpectedly"))
    }
}

impl<T: Failable> Pending<T> {
    /// Returns the value once ready (only once). A task that panicked reports an error instead of staying
    /// pending forever.
    pub fn poll(&mut self) -> Option<T> {
        let rx = self.rx.as_mut()?;
        match rx.try_recv() {
            Ok(v) => {
                self.rx = None;
                Some(v)
            }
            Err(tokio::sync::oneshot::error::TryRecvError::Empty) => None,
            Err(tokio::sync::oneshot::error::TryRecvError::Closed) => {
                self.rx = None;
                Some(T::task_failed())
            }
        }
    }
}

/// Spawns `fut` and requests a repaint when it completes.
pub fn spawn<T: Send + 'static>(ctx: &egui::Context, fut: impl Future<Output = T> + Send + 'static) -> Pending<T> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    let ctx = ctx.clone();
    handle().spawn(async move {
        let v = fut.await;
        let _ = tx.send(v);
        ctx.request_repaint();
    });
    Pending { rx: Some(rx) }
}

/// Fire-and-forget: nothing reports the outcome, so the future must handle (or log) its own errors.
pub fn spawn_detached(fut: impl Future<Output = ()> + Send + 'static) {
    handle().spawn(fut);
}
