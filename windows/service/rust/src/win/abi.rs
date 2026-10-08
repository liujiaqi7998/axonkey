//! Public driver ABI. All fields have integer representations; no packed references.
use std::mem::{align_of, offset_of, size_of};

pub const HID_VERSION: u32 = 8;
pub const MIC_VERSION: u32 = 1;
pub const MAX_REPORT: u32 = 65535;
pub const RING_BYTES: u32 = 98304;
const fn ctl(function: u32, method: u32, access: u32) -> u32 {
    (0x22 << 16) | (access << 14) | (function << 2) | method
}
pub const QUERY_IDENTITY: u32 = ctl(0x800, 0, 1);
pub const HID_DATA: u32 = ctl(0x802, 2, 1);
pub const SET_FORWARD: u32 = ctl(0x806, 0, 3);
pub const SET_BLOCK: u32 = ctl(0x807, 0, 3);
pub const MAP_RING: u32 = ctl(0x900, 0, 3);
pub const MIC_START: u32 = ctl(0x901, 0, 3);
pub const MIC_STOP: u32 = ctl(0x902, 0, 3);
pub const MIC_RESET: u32 = ctl(0x903, 0, 3);
pub const QUERY_STATE: u32 = ctl(0x904, 0, 1);
pub const COMMIT: u32 = ctl(0x905, 0, 3);

#[repr(C)]
#[derive(Default, Copy, Clone)]
pub struct Identity {
    pub size: u32,
    pub vendor: u16,
    pub product: u16,
    pub version: u32,
    pub collection: u32,
    pub max_report: u32,
    pub forwarding: u8,
    pub blocked: u8,
    pub remapped: u8,
    pub reserved: u8,
}
#[repr(C)]
#[derive(Default, Copy, Clone)]
pub struct Switch {
    pub enabled: u32,
}
#[repr(C)]
#[derive(Default, Copy, Clone)]
pub struct Mapping {
    pub size: u32,
    pub version: u32,
    pub address: u64,
    pub buffer_bytes: u32,
    pub sample_rate: u32,
    pub channels: u32,
    pub bits: u32,
    pub block_align: u32,
    pub reserved: u32,
    pub generation: u64,
}
#[repr(C)]
#[derive(Default, Copy, Clone, Debug)]
pub struct Commit {
    pub size: u32,
    pub version: u32,
    pub generation: u64,
    pub write_position: u64,
    pub byte_count: u32,
    pub reserved: u32,
}
#[repr(C)]
#[derive(Default, Copy, Clone, Debug)]
pub struct MicState {
    pub size: u32,
    pub version: u32,
    pub flags: u32,
    pub buffer_bytes: u32,
    pub sample_rate: u32,
    pub channels: u32,
    pub bits: u32,
    pub block_align: u32,
    pub generation: u64,
    pub write_position: u64,
    pub read_position: u64,
    pub available: u32,
    pub free: u32,
    pub forwarded: u64,
    pub silence: u64,
}

// Sealed: only integer-only, fully specified driver layouts may receive arbitrary IOCTL bytes.
mod sealed {
    pub trait Sealed {}
}
pub trait Wire: sealed::Sealed + Copy + Default {}
macro_rules! wire { ($($ty:ty),*) => { $(impl sealed::Sealed for $ty {} impl Wire for $ty {})* }; }
wire!(Identity, Switch, Mapping, Commit, MicState);
const _: () = {
    assert!(size_of::<Identity>() == 24);
    assert!(size_of::<Switch>() == 4);
    assert!(size_of::<Mapping>() == 48);
    assert!(size_of::<Commit>() == 32);
    assert!(size_of::<MicState>() == 80);
};

/// Remap is storage-only. Encode explicitly, avoiding padding/uninitialized byte views.
pub const REMAP_BYTES: usize = 1040;
pub fn empty_remap() -> Vec<u8> {
    let mut bytes = vec![0; REMAP_BYTES];
    bytes[..4].copy_from_slice(&(REMAP_BYTES as u32).to_le_bytes());
    bytes[4..8].copy_from_slice(&HID_VERSION.to_le_bytes());
    bytes
}
pub fn valid_remap(bytes: &[u8]) -> bool {
    if bytes.len() != REMAP_BYTES {
        return false;
    }
    let word = |i| u32::from_le_bytes(bytes[i..i + 4].try_into().expect("checked remap size"));
    if word(0) != REMAP_BYTES as u32 || word(4) != HID_VERSION || word(8) > 256 || word(12) != 0 {
        return false;
    }
    let count = word(8) as usize;
    let mut sources = std::collections::BTreeSet::new();
    for (i, item) in bytes[16..].as_chunks::<4>().0.iter().enumerate() {
        let source = u16::from_le_bytes([item[0], item[1]]);
        let target = u16::from_le_bytes([item[2], item[3]]);
        if i >= count {
            if source != 0 || target != 0 {
                return false;
            }
        } else if source < 4
            || (target != 0 && target < 4)
            || source == target
            || !sources.insert(source)
        {
            return false;
        }
    }
    true
}

pub fn verify_layouts() -> bool {
    unsafe extern "C" {
        fn axonkey_abi_value(index: u32) -> usize;
    }
    let expected = [
        size_of::<Identity>(),
        align_of::<Identity>(),
        offset_of!(Identity, version),
        offset_of!(Identity, max_report),
        size_of::<Switch>(),
        REMAP_BYTES,
        16,
        size_of::<Mapping>(),
        align_of::<Mapping>(),
        offset_of!(Mapping, address),
        offset_of!(Mapping, generation),
        size_of::<Commit>(),
        offset_of!(Commit, write_position),
        size_of::<MicState>(),
        offset_of!(MicState, generation),
        offset_of!(MicState, available),
        offset_of!(MicState, silence),
        QUERY_IDENTITY as usize,
        HID_DATA as usize,
        SET_FORWARD as usize,
        SET_BLOCK as usize,
        MAP_RING as usize,
        MIC_START as usize,
        MIC_STOP as usize,
        MIC_RESET as usize,
        QUERY_STATE as usize,
        COMMIT as usize,
        HID_VERSION as usize,
        MAX_REPORT as usize,
    ];
    expected.iter().enumerate().all(|(i, value)| {
        // SAFETY: pure C function returns a scalar; all indices are within its table.
        *value == unsafe { axonkey_abi_value(i as u32) }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn public_c_header_layouts_and_ioctl_numbers_match() {
        assert!(verify_layouts());
    }
    #[test]
    fn remap_rejects_duplicate_and_unused_entries() {
        let mut bytes = empty_remap();
        assert!(valid_remap(&bytes));
        bytes[16] = 4;
        assert!(!valid_remap(&bytes));
        bytes[8] = 1;
        assert!(valid_remap(&bytes));
        bytes[8] = 2;
        bytes[20] = 4;
        assert!(!valid_remap(&bytes));
        bytes[20] = 5;
        assert!(valid_remap(&bytes));
        bytes[12] = 1;
        assert!(!valid_remap(&bytes));
    }
}
