#![forbid(unsafe_code)]
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::Notify;

#[derive(Default)]
pub struct Cancel {
    requested: AtomicBool,
    wake: Notify,
}
impl Cancel {
    pub fn request(&self) {
        self.requested.store(true, Ordering::Release);
        self.wake.notify_waiters();
    }
    pub fn is_requested(&self) -> bool {
        self.requested.load(Ordering::Acquire)
    }
    pub async fn cancelled(&self) {
        loop {
            let wake = self.wake.notified();
            tokio::pin!(wake);
            wake.as_mut().enable();
            if self.is_requested() {
                return;
            }
            wake.await;
        }
    }
}
