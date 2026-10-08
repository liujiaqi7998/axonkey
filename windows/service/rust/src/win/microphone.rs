use super::{
    abi::*,
    handles::{own, raw},
};
use crate::{audio::session::Microphone, cancel::Cancel, Error, Result};
use std::{
    mem::size_of,
    os::windows::io::OwnedHandle,
    sync::Arc,
    thread,
    time::{Duration, Instant},
};
use windows::{
    core::w,
    Win32::{
        Foundation::{GENERIC_READ, GENERIC_WRITE},
        Storage::FileSystem::{CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_MODE, OPEN_EXISTING},
        System::IO::DeviceIoControl,
    },
};

pub trait Transport {
    fn open_map(&mut self) -> Result<Mapping>;
    fn query(&mut self) -> Result<MicState>;
    fn command(&mut self, code: u32) -> Result<()>;
    fn copy(&mut self, position: u32, samples: &[i16]) -> Result<()>;
    fn commit(&mut self, commit: &Commit) -> Result<()>;
    fn close(&mut self);
}
#[derive(Default)]
pub struct Driver {
    handle: Option<OwnedHandle>,
    mapping: Option<Mapping>,
}
impl Driver {
    fn query_value<T: Wire>(&self, code: u32) -> Result<T> {
        let handle = self
            .handle
            .as_ref()
            .ok_or_else(|| Error::new(6, "Microphone handle closed"))?;
        let mut value = T::default();
        let mut returned = 0;
        // SAFETY: T is a sealed integer-only driver layout. Buffer is fully initialized and lives through synchronous I/O.
        unsafe {
            DeviceIoControl(
                raw(handle),
                code,
                None,
                0,
                Some((&mut value as *mut T).cast()),
                size_of::<T>() as u32,
                Some(&mut returned),
                None,
            )
        }
        .map_err(|_| Error::last("Microphone query failed"))?;
        if returned as usize != size_of::<T>() {
            return Err(Error::new(1306, "Microphone response length mismatch"));
        }
        Ok(value)
    }
}
impl Transport for Driver {
    fn open_map(&mut self) -> Result<Mapping> {
        self.close();
        // SAFETY: constant device path; synchronous, exclusive open with no external pointers.
        let handle = unsafe {
            CreateFileW(
                w!(r"\\.\QuarborVirtualMicrophone"),
                GENERIC_READ.0 | GENERIC_WRITE.0,
                FILE_SHARE_MODE(0),
                None,
                OPEN_EXISTING,
                FILE_ATTRIBUTE_NORMAL,
                None,
            )
        }
        .map_err(|_| Error::last("Open exclusive virtual microphone"))?;
        // SAFETY: CreateFileW returned a new valid kernel handle.
        self.handle = Some(unsafe { own(handle) });
        match self.query_value(MAP_RING) {
            Ok(mapping) => {
                self.mapping = Some(mapping);
                Ok(mapping)
            }
            Err(e) => {
                self.close();
                Err(e)
            }
        }
    }
    fn query(&mut self) -> Result<MicState> {
        self.query_value(QUERY_STATE)
    }
    fn command(&mut self, code: u32) -> Result<()> {
        let handle = self
            .handle
            .as_ref()
            .ok_or_else(|| Error::new(6, "Microphone handle closed"))?;
        let mut returned = 0;
        // SAFETY: these synchronous commands have no input or output buffers.
        unsafe {
            DeviceIoControl(
                raw(handle),
                code,
                None,
                0,
                None,
                0,
                Some(&mut returned),
                None,
            )
        }
        .map_err(|_| Error::last("Microphone command failed"))?;
        Ok(())
    }
    fn copy(&mut self, position: u32, samples: &[i16]) -> Result<()> {
        let map = self
            .mapping
            .filter(|_| self.handle.is_some())
            .ok_or_else(|| Error::new(6, "Microphone mapping closed"))?;
        let bytes = std::mem::size_of_val(samples);
        if (position as usize)
            .checked_add(bytes)
            .is_none_or(|end| end > map.buffer_bytes as usize)
        {
            return Err(Error::new(13, "Microphone copy exceeds mapping"));
        }
        let address = (map.address as usize)
            .checked_add(position as usize)
            .ok_or_else(|| Error::new(13, "Mapping address overflow"))?;
        unsafe extern "C" {
            fn axonkey_guarded_copy(
                destination: *mut core::ffi::c_void,
                source: *const core::ffi::c_void,
                bytes: usize,
            ) -> u32;
        }
        // SAFETY: source is a live PCM16 slice on little-endian x64. The driver mapping is owned by the still-open handle;
        // bounds were checked. C/SEH contains access faults and issues MemoryBarrier before returning success.
        let error =
            unsafe { axonkey_guarded_copy(address as *mut _, samples.as_ptr().cast(), bytes) };
        if error != 0 {
            return Err(Error::new(error, "Microphone mapping copy failed"));
        }
        Ok(())
    }
    fn commit(&mut self, commit: &Commit) -> Result<()> {
        let handle = self
            .handle
            .as_ref()
            .ok_or_else(|| Error::new(6, "Microphone handle closed"))?;
        let mut returned = 0;
        // SAFETY: Commit is a fully initialized C layout; driver copies it synchronously.
        unsafe {
            DeviceIoControl(
                raw(handle),
                COMMIT,
                Some((commit as *const Commit).cast()),
                size_of::<Commit>() as u32,
                None,
                0,
                Some(&mut returned),
                None,
            )
        }
        .map_err(|_| Error::last("Microphone COMMIT failed"))?;
        Ok(())
    }
    fn close(&mut self) {
        self.mapping = None;
        self.handle = None;
    }
}

pub struct Sink<T: Transport = Driver> {
    transport: T,
    cancel: Arc<Cancel>,
    opened: bool,
    started: bool,
    buffer_bytes: u32,
    committed: u64,
    stall_timeout: Duration,
}
impl Sink<Driver> {
    pub fn driver(cancel: Arc<Cancel>) -> Self {
        Self::new(Driver::default(), cancel)
    }
}
impl<T: Transport> Sink<T> {
    pub fn new(transport: T, cancel: Arc<Cancel>) -> Self {
        Self {
            transport,
            cancel,
            opened: false,
            started: false,
            buffer_bytes: 0,
            committed: 0,
            stall_timeout: Duration::from_millis(250),
        }
    }
    fn close(&mut self) {
        self.transport.close();
        self.opened = false;
        self.started = false;
        self.buffer_bytes = 0;
    }
    fn state(&mut self) -> Result<MicState> {
        let state = self.transport.query()?;
        let available = state.write_position.checked_sub(state.read_position);
        if state.size as usize != size_of::<MicState>()
            || state.version != MIC_VERSION
            || state.generation == 0
            || state.buffer_bytes != self.buffer_bytes
            || state.sample_rate != 48000
            || state.channels != 1
            || state.bits != 16
            || state.block_align != 2
            || state.flags & 7 != 7
            || available != Some(u64::from(state.available))
            || state.available > self.buffer_bytes
            || state.free != self.buffer_bytes - state.available
            || (state.write_position | state.read_position | u64::from(state.free)) & 1 != 0
        {
            return Err(Error::new(13, "Invalid/offline microphone QUERY_STATE"));
        }
        Ok(state)
    }
}
impl<T: Transport> Microphone for Sink<T> {
    fn start(&mut self) -> Result<()> {
        if self.cancel.is_requested() {
            return Err(Error::new(995, "Microphone cancelled"));
        }
        if self.started {
            return Ok(());
        }
        let mapping = match self.transport.open_map() {
            Ok(mapping) => mapping,
            Err(e) => {
                self.close();
                return Err(e);
            }
        };
        self.opened = true;
        if mapping.size as usize != size_of::<Mapping>()
            || mapping.version != MIC_VERSION
            || mapping.address == 0
            || mapping.buffer_bytes == 0
            || mapping.buffer_bytes > RING_BYTES
            || mapping.buffer_bytes & 1 != 0
            || mapping.sample_rate != 48000
            || mapping.channels != 1
            || mapping.bits != 16
            || mapping.block_align != 2
        {
            self.close();
            return Err(Error::new(
                1306,
                "Incompatible microphone MAP_RING; expected 48 kHz mono PCM16",
            ));
        }
        self.buffer_bytes = mapping.buffer_bytes;
        let result = if self.cancel.is_requested() {
            Err(Error::new(995, "Microphone cancelled"))
        } else {
            self.transport
                .command(MIC_RESET)
                .and_then(|()| self.transport.command(MIC_START))
        };
        if let Err(e) = result {
            self.close();
            return Err(e);
        }
        self.started = true;
        self.committed = 0;
        log::info!("PCM ingress started (48000 Hz mono PCM16)");
        Ok(())
    }
    fn reset(&mut self) -> Result<()> {
        if self.cancel.is_requested() || !self.started {
            return Err(Error::new(995, "Microphone cancelled/closed"));
        }
        self.transport.command(MIC_RESET)
    }
    fn push(&mut self, samples: &[i16]) -> Result<()> {
        if !self.started {
            return Err(Error::new(6, "Microphone not started"));
        }
        let mut offset = 0;
        let mut deadline = Instant::now() + self.stall_timeout;
        while offset < samples.len() {
            if self.cancel.is_requested() {
                return Err(Error::new(995, "Microphone cancelled"));
            }
            let state = self.state()?;
            let count = 480
                .min((state.free / 2) as usize)
                .min(samples.len() - offset);
            if count == 0 {
                if Instant::now() >= deadline {
                    return Err(Error::new(1460, "Microphone ring stalled for 250 ms"));
                }
                thread::sleep(Duration::from_millis(1));
                continue;
            }
            let position = (state.write_position % u64::from(self.buffer_bytes)) as u32;
            let first = count.min(((self.buffer_bytes - position) / 2) as usize);
            self.transport
                .copy(position, &samples[offset..offset + first])?;
            if first < count {
                self.transport
                    .copy(0, &samples[offset + first..offset + count])?;
            }
            self.transport.commit(&Commit {
                size: size_of::<Commit>() as u32,
                version: MIC_VERSION,
                generation: state.generation,
                write_position: state.write_position,
                byte_count: (count * 2) as u32,
                reserved: 0,
            })?;
            offset += count;
            self.committed += (count * 2) as u64;
            deadline = Instant::now() + self.stall_timeout;
        }
        Ok(())
    }
    fn stop(&mut self, drain: bool) {
        if !self.opened {
            return;
        }
        let deadline = Instant::now() + self.stall_timeout;
        if self.started {
            while let Ok(state) = self.state() {
                if !drain
                    || self.cancel.is_requested()
                    || state.available == 0
                    || Instant::now() >= deadline
                {
                    log::info!(
                        "PCM stop: committed={}, forwarded={}, queued={}, silence={}",
                        self.committed,
                        state.forwarded,
                        state.available,
                        state.silence
                    );
                    break;
                }
                thread::sleep(Duration::from_millis(1));
            }
        }
        if let Err(e) = self.transport.command(MIC_STOP) {
            log::warn!("Microphone STOP failed: {e}");
        }
        self.close();
    }
}
impl<T: Transport> Drop for Sink<T> {
    fn drop(&mut self) {
        self.stop(false);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[derive(Default)]
    struct Fake {
        data: Vec<i16>,
        state: MicState,
        copies: Vec<(u32, usize)>,
        commits: Vec<Commit>,
        closes: usize,
        fail: Option<u32>,
        fail_copy: bool,
    }
    impl Transport for Fake {
        fn open_map(&mut self) -> Result<Mapping> {
            if self.fail == Some(MAP_RING) {
                return Err(Error::new(31, "map failure"));
            }
            self.data = vec![0; 8];
            self.state = MicState {
                size: 80,
                version: 1,
                flags: 7,
                buffer_bytes: 16,
                sample_rate: 48000,
                channels: 1,
                bits: 16,
                block_align: 2,
                generation: 1,
                write_position: 12,
                read_position: 12,
                free: 16,
                ..Default::default()
            };
            Ok(Mapping {
                size: 48,
                version: 1,
                address: 1,
                buffer_bytes: 16,
                sample_rate: 48000,
                channels: 1,
                bits: 16,
                block_align: 2,
                ..Default::default()
            })
        }
        fn query(&mut self) -> Result<MicState> {
            if self.fail == Some(QUERY_STATE) {
                return Err(Error::new(31, "query failure"));
            }
            Ok(self.state)
        }
        fn command(&mut self, code: u32) -> Result<()> {
            if self.fail == Some(code) {
                return Err(Error::new(31, "command failure"));
            }
            Ok(())
        }
        fn copy(&mut self, position: u32, samples: &[i16]) -> Result<()> {
            if self.fail_copy {
                return Err(Error::new(0xc0000005, "copy fault"));
            }
            self.copies.push((position, samples.len()));
            let offset = position as usize / 2;
            self.data[offset..offset + samples.len()].copy_from_slice(samples);
            Ok(())
        }
        fn commit(&mut self, commit: &Commit) -> Result<()> {
            if self.fail == Some(COMMIT) {
                return Err(Error::new(31, "commit failure"));
            }
            self.commits.push(*commit);
            self.state.write_position += u64::from(commit.byte_count);
            self.state.free -= commit.byte_count;
            self.state.available += commit.byte_count;
            Ok(())
        }
        fn close(&mut self) {
            self.closes += 1;
        }
    }
    #[test]
    fn ring_wrap_commit_failure_cancel_and_close() {
        let cancel = Arc::new(Cancel::default());
        let mut sink = Sink::new(Fake::default(), cancel.clone());
        sink.start().unwrap();
        sink.push(&[1, 2, 3, 4]).unwrap();
        assert_eq!(sink.transport.copies, vec![(12, 2), (0, 2)]);
        assert_eq!(sink.transport.data, vec![3, 4, 0, 0, 0, 0, 1, 2]);
        assert_eq!(sink.transport.commits[0].write_position, 12);
        sink.transport.fail = Some(COMMIT);
        assert!(sink.push(&[5]).is_err());
        cancel.request();
        assert!(sink.push(&[6]).is_err());
        sink.stop(true);
        assert_eq!(sink.transport.closes, 1);
        assert!(sink.start().is_err());
    }
    #[test]
    fn partial_start_and_stall_release_mapping() {
        let mut sink = Sink::new(
            Fake {
                fail: Some(MIC_START),
                ..Default::default()
            },
            Arc::new(Cancel::default()),
        );
        assert!(sink.start().is_err());
        assert_eq!(sink.transport.closes, 1);
        sink.transport.fail = None;
        sink.start().unwrap();
        sink.stall_timeout = Duration::from_millis(2);
        sink.push(&[0; 8]).unwrap();
        assert_eq!(sink.push(&[1]).unwrap_err().native, 1460);
        sink.transport.state.generation = 0;
        assert!(sink.push(&[1]).is_err());
        sink.stop(false);
        assert_eq!(sink.transport.closes, 2);
    }
    #[test]
    fn every_driver_failure_releases_owner_and_never_commits_failed_copy() {
        for failure in [MAP_RING, MIC_RESET, MIC_START] {
            let mut sink = Sink::new(
                Fake {
                    fail: Some(failure),
                    ..Default::default()
                },
                Arc::new(Cancel::default()),
            );
            assert!(sink.start().is_err());
            assert_eq!(sink.transport.closes, 1);
            sink.stop(false);
            assert_eq!(sink.transport.closes, 1);
        }
        for failure in [QUERY_STATE, COMMIT, MIC_STOP] {
            let mut sink = Sink::new(Fake::default(), Arc::new(Cancel::default()));
            sink.start().unwrap();
            sink.transport.fail = Some(failure);
            assert_eq!(sink.push(&[1]).is_err(), failure != MIC_STOP);
            sink.stop(false);
            assert_eq!(sink.transport.closes, 1);
        }
        let mut sink = Sink::new(Fake::default(), Arc::new(Cancel::default()));
        sink.start().unwrap();
        sink.transport.fail_copy = true;
        assert!(sink.push(&[1]).is_err());
        assert!(sink.transport.commits.is_empty());
        sink.stop(false);
        assert_eq!(sink.transport.closes, 1);
    }
    #[test]
    fn malformed_driver_state_never_reaches_the_mapped_buffer() {
        let invalid: &[fn(&mut MicState)] = &[
            |s| s.size = 79,
            |s| s.version = 2,
            |s| s.flags = 3,
            |s| s.buffer_bytes = 0,
            |s| s.sample_rate = 16000,
            |s| s.channels = 2,
            |s| s.bits = 24,
            |s| s.block_align = 1,
            |s| s.generation = 0,
            |s| s.write_position = 11,
            |s| s.read_position = 13,
            |s| s.available = 17,
            |s| s.free = 15,
        ];
        for mutate in invalid {
            let mut sink = Sink::new(Fake::default(), Arc::new(Cancel::default()));
            sink.start().unwrap();
            mutate(&mut sink.transport.state);
            assert!(sink.push(&[1]).is_err());
            assert!(sink.transport.copies.is_empty());
            assert!(sink.transport.commits.is_empty());
            sink.stop(false);
            assert_eq!(sink.transport.closes, 1);
        }
    }
    #[test]
    fn guarded_copy_contains_access_fault_without_unwinding_rust() {
        unsafe extern "C" {
            fn axonkey_guarded_copy(
                destination: *mut core::ffi::c_void,
                source: *const core::ffi::c_void,
                bytes: usize,
            ) -> u32;
        }
        let samples = [1_i16, -32768, 32767];
        let mut output = [0_i16; 3];
        // SAFETY: valid disjoint buffers; the C function performs its own SEH boundary.
        let result =
            unsafe { axonkey_guarded_copy(output.as_mut_ptr().cast(), samples.as_ptr().cast(), 6) };
        assert_eq!(result, 0);
        assert_eq!(samples, output);
        // SAFETY: deliberate fault injection into the C/SEH adapter's documented fault-containment path.
        // No Rust code dereferences this null pointer, nor can the C exception unwind into Rust.
        let result =
            unsafe { axonkey_guarded_copy(std::ptr::null_mut(), samples.as_ptr().cast(), 6) };
        assert_eq!(result, 0xc0000005);
    }
}
