pub mod quic;
pub mod udp;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;

/// Handle to one node's transport worker thread. Dropping stops + joins it.
pub struct Worker {
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl Worker {
    pub fn spawn(name: String, f: impl FnOnce(Arc<AtomicBool>) + Send + 'static) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let stop_for_thread = stop.clone();
        let handle = std::thread::Builder::new()
            .name(name)
            .spawn(move || f(stop_for_thread))
            .expect("spawn net worker thread");
        Self { stop, handle: Some(handle) }
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            // Workers poll `stop` at millisecond cadence (socket read timeouts),
            // so this join is bounded and safe from the proc thread.
            let _ = h.join();
        }
    }
}

pub(crate) fn should_stop(stop: &AtomicBool) -> bool {
    stop.load(Ordering::Relaxed)
}
