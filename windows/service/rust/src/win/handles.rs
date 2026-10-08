use crate::Result;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use windows::{
    core::PCWSTR,
    Win32::{
        Foundation::{HANDLE, WAIT_OBJECT_0},
        System::{
            Threading::{CreateEventW, ResetEvent, SetEvent, WaitForSingleObject},
            WinRT::{RoInitialize, RoUninitialize, RO_INIT_MULTITHREADED},
        },
    },
};

pub fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(Some(0)).collect()
}
pub fn raw(handle: &OwnedHandle) -> HANDLE {
    HANDLE(handle.as_raw_handle())
}
/// # Safety
/// `handle` is a newly acquired non-null, non-invalid kernel handle, owned exclusively by this call.
pub unsafe fn own(handle: HANDLE) -> OwnedHandle {
    // SAFETY: required by this function's contract. OwnedHandle closes exactly once.
    unsafe { OwnedHandle::from_raw_handle(handle.0) }
}
pub struct Event(OwnedHandle);
impl Event {
    pub fn new() -> Result<Self> {
        // SAFETY: unnamed manual-reset event with no borrowed security descriptor.
        let handle = unsafe { CreateEventW(None, true, false, PCWSTR::null())? };
        // SAFETY: CreateEventW returned a new valid handle.
        Ok(Self(unsafe { own(handle) }))
    }
    pub fn handle(&self) -> HANDLE {
        raw(&self.0)
    }
    pub fn set(&self) {
        // SAFETY: event is owned and remains live throughout the call.
        unsafe {
            let _ = SetEvent(self.handle());
        }
    }
    pub fn reset(&self) -> Result<()> {
        // SAFETY: event is owned and remains live throughout the call.
        unsafe {
            ResetEvent(self.handle())?;
        }
        Ok(())
    }
    pub fn signaled(&self) -> bool {
        // SAFETY: polling a live event never transfers ownership.
        unsafe { WaitForSingleObject(self.handle(), 0) == WAIT_OBJECT_0 }
    }
}
pub struct Apartment(std::marker::PhantomData<std::rc::Rc<()>>);
impl Apartment {
    pub fn mta() -> Result<Self> {
        // SAFETY: called on the owning worker thread; Drop balances successful initialization.
        unsafe {
            RoInitialize(RO_INIT_MULTITHREADED)?;
        }
        Ok(Self(std::marker::PhantomData))
    }
}
impl Drop for Apartment {
    fn drop(&mut self) {
        // SAFETY: !Send/!Sync marker keeps this guard on its initialized thread.
        unsafe {
            RoUninitialize();
        }
    }
}
