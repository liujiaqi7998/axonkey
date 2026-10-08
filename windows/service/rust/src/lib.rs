#![deny(unsafe_op_in_unsafe_fn)]

pub mod audio;
pub mod cancel;
pub mod config;
pub mod error;
pub mod logging;
pub mod rpc;
pub mod service;
pub mod win;

pub mod proto {
    include!(concat!(env!("OUT_DIR"), "/axonkey.service.v1.rs"));
}

pub use error::{Error, Result};

pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u64::MAX as u128) as u64
}

pub(crate) fn lock<T>(mutex: &std::sync::Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    // Workers report panics at their boundary; cleanup must still recover owned resources.
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}
