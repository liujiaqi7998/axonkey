use super::{
    abi::*,
    device_setup::EndpointInfo,
    handles::{own, raw, wide, Event},
};
use crate::{rpc::Hub, Error, Result};
use std::{
    mem::size_of,
    os::windows::io::OwnedHandle,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    thread,
};
use windows::{
    core::PCWSTR,
    Win32::{
        Foundation::{GetLastError, ERROR_IO_PENDING, GENERIC_READ, GENERIC_WRITE, WAIT_OBJECT_0},
        Storage::FileSystem::{CreateFileW, FILE_FLAG_OVERLAPPED, FILE_SHARE_MODE, OPEN_EXISTING},
        System::{
            Threading::WaitForMultipleObjects,
            IO::{CancelIoEx, DeviceIoControl, GetOverlappedResult, OVERLAPPED},
        },
    },
};

struct Operation {
    event: Event,
    overlapped: Box<OVERLAPPED>,
}
impl Operation {
    fn new() -> Result<Self> {
        Ok(Self {
            event: Event::new()?,
            overlapped: Box::new(OVERLAPPED::default()),
        })
    }
    fn transfer(
        &mut self,
        handle: &OwnedHandle,
        code: u32,
        input: Option<&Switch>,
        output: &mut [u8],
        stop: Option<&Event>,
    ) -> Result<usize> {
        self.event.reset()?;
        *self.overlapped = OVERLAPPED {
            hEvent: self.event.handle(),
            ..Default::default()
        };
        let mut returned = 0;
        // SAFETY: input/output/OVERLAPPED and event stay alive and stationary until completion, including the cancel-and-drain path below.
        let result = unsafe {
            DeviceIoControl(
                raw(handle),
                code,
                input.map(|v| (v as *const Switch).cast()),
                if input.is_some() {
                    size_of::<Switch>() as u32
                } else {
                    0
                },
                (!output.is_empty()).then_some(output.as_mut_ptr().cast()),
                output.len() as u32,
                Some(&mut returned),
                Some(&mut *self.overlapped),
            )
        };
        if result.is_ok() {
            return Ok(returned as usize);
        }
        // SAFETY: capture API error without intervening calls.
        if unsafe { GetLastError() } != ERROR_IO_PENDING {
            return Err(Error::last("HID DeviceIoControl"));
        }
        // No allocation or user callback may unwind while the driver borrows the buffers.
        let waits = [
            self.event.handle(),
            stop.map_or(self.event.handle(), Event::handle),
        ];
        let waits = &waits[..if stop.is_some() { 2 } else { 1 }];
        // SAFETY: event handles remain owned throughout the wait; only this worker owns the operation.
        let completed = unsafe {
            WaitForMultipleObjects(waits, false, if stop.is_some() { u32::MAX } else { 5000 })
        } == WAIT_OBJECT_0;
        if !completed {
            // SAFETY: cancellation does not release the operation. Drain even if cancellation reports NOT_FOUND (completion race).
            unsafe {
                let _ = CancelIoEx(raw(handle), Some(&*self.overlapped));
                let _ = GetOverlappedResult(raw(handle), &*self.overlapped, &mut returned, true);
            }
            return Err(Error::new(
                if stop.is_some_and(Event::signaled) {
                    995
                } else {
                    1460
                },
                "HID operation cancelled/timed out",
            ));
        }
        // SAFETY: the operation signaled completion; all borrowed buffers remain valid.
        unsafe { GetOverlappedResult(raw(handle), &*self.overlapped, &mut returned, false) }
            .map_err(|_| Error::last("HID completion"))?;
        Ok(returned as usize)
    }
}
struct Device {
    handle: OwnedHandle,
    control: Operation,
}
impl Device {
    fn switch(&mut self, code: u32, enabled: bool) -> Result<()> {
        self.control.transfer(
            &self.handle,
            code,
            Some(&Switch {
                enabled: u32::from(enabled),
            }),
            &mut [],
            None,
        )?;
        Ok(())
    }
}
impl Drop for Device {
    fn drop(&mut self) {
        let _ = self.switch(SET_BLOCK, false);
        let _ = self.switch(SET_FORWARD, false);
    }
}

pub struct Endpoint {
    pub info: EndpointInfo,
    stop: Arc<Event>,
    valid: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
}
impl Endpoint {
    pub fn start(info: EndpointInfo, hub: Arc<Hub>) -> Result<Self> {
        let stop = Arc::new(Event::new()?);
        let valid = Arc::new(AtomicBool::new(false));
        let (ready, startup) = std::sync::mpsc::sync_channel(1);
        let worker_info = info.clone();
        let worker_stop = stop.clone();
        let worker_valid = valid.clone();
        let worker = thread::Builder::new()
            .name("axonkey-hid".into())
            .spawn(move || {
                let result =
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
                        let path = wide(&worker_info.path);
                        // SAFETY: null-terminated path; exclusive overlapped device handle, no borrowed attributes.
                        let handle = unsafe {
                            CreateFileW(
                                PCWSTR(path.as_ptr()),
                                GENERIC_READ.0 | GENERIC_WRITE.0,
                                FILE_SHARE_MODE(0),
                                None,
                                OPEN_EXISTING,
                                FILE_FLAG_OVERLAPPED,
                                None,
                            )
                        }
                        .map_err(|_| Error::last("Open HID endpoint"))?;
                        // SAFETY: CreateFileW returned a new valid kernel handle.
                        let handle = unsafe { own(handle) };
                        let mut device = Device {
                            handle,
                            control: Operation::new()?,
                        };
                        let mut identity = [0_u8; 24];
                        let returned = device.control.transfer(
                            &device.handle,
                            QUERY_IDENTITY,
                            None,
                            &mut identity,
                            None,
                        )?;
                        let word = |at| {
                            u32::from_le_bytes(
                                identity[at..at + 4]
                                    .try_into()
                                    .expect("fixed identity size"),
                            )
                        };
                        if returned != 24
                            || word(0) != 24
                            || word(8) != HID_VERSION
                            || !(1..=MAX_REPORT).contains(&word(16))
                        {
                            return Err(Error::new(1306, "Incompatible HID QUERY_IDENTITY"));
                        }
                        let mut report = vec![0_u8; word(16) as usize];
                        let mut read = Operation::new()?;
                        device.switch(SET_FORWARD, true)?;
                        device.switch(SET_BLOCK, true)?;
                        worker_valid.store(true, Ordering::Release);
                        let _ = ready.send(Ok(()));
                        while !worker_stop.signaled() {
                            let bytes = read.transfer(
                                &device.handle,
                                HID_DATA,
                                None,
                                &mut report,
                                Some(&worker_stop),
                            )?;
                            if bytes > report.len() {
                                return Err(Error::new(
                                    13,
                                    "HID returned report beyond output buffer",
                                ));
                            }
                            if bytes != 0 {
                                hub.keyboard(&worker_info.instance, report[..bytes].to_vec());
                            }
                        }
                        Ok(())
                    }));
                worker_valid.store(false, Ordering::Release);
                // The Device guard has already cancelled/drained reads and cleared blocking before reset is published.
                hub.keyboard(&worker_info.instance, Vec::new());
                let error = match result {
                    Ok(Err(e)) if e.native != 995 => Some(e),
                    Err(_) => Some(Error::new(574, "HID worker panicked")),
                    _ => None,
                };
                if let Some(error) = error {
                    let _ = ready.send(Err(error.clone()));
                    hub.issue("hid_filter_driver_error", &worker_info.instance, &error);
                }
            })?;
        match startup.recv() {
            Ok(Ok(())) => Ok(Self {
                info,
                stop,
                valid,
                worker: Some(worker),
            }),
            result => {
                stop.set();
                let _ = worker.join();
                Err(result
                    .ok()
                    .and_then(std::result::Result::err)
                    .unwrap_or_else(|| Error::new(31, "HID worker ended during startup")))
            }
        }
    }
    pub fn valid(&self) -> bool {
        self.valid.load(Ordering::Acquire) && self.worker.as_ref().is_some_and(|w| !w.is_finished())
    }
    pub fn request_stop(&self) {
        self.stop.set();
    }
    pub fn join(&mut self) {
        self.request_stop();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}
impl Drop for Endpoint {
    fn drop(&mut self) {
        self.join();
    }
}
