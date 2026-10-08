use super::handles::wide;
use crate::{Error, Result};
use std::mem::size_of;
use windows::{
    core::{GUID, PCWSTR},
    Win32::{
        Devices::DeviceAndDriverInstallation::*,
        Foundation::{
            GetLastError, ERROR_FILE_NOT_FOUND, ERROR_INSUFFICIENT_BUFFER, ERROR_INVALID_DATA,
            ERROR_NO_MORE_ITEMS,
        },
        System::Registry::{REG_MULTI_SZ, REG_SZ},
    },
};

const KEYBOARD: GUID = GUID::from_u128(0x4d36e96b_e325_11ce_bfc1_08002be10318);
pub const HID_INTERFACE: GUID = GUID::from_u128(0x8e7f4d20_0f76_4c4b_916c_1b4b0f7d6318);
const FILTER: &str = "QuarborHIDFilterDriver";
const MAX_PROPERTY: usize = 1024 * 1024;
pub fn is_rc003(id: &str) -> bool {
    let id = id.to_ascii_lowercase();
    (id.contains("vid_2717") || id.contains("vid&012717"))
        && (id.contains("pid_32b8") || id.contains("pid&32b8"))
}
fn text(units: &[u16]) -> String {
    String::from_utf16_lossy(&units[..units.iter().position(|u| *u == 0).unwrap_or(units.len())])
}
fn strings(bytes: &[u8]) -> Result<Vec<String>> {
    if !bytes.len().is_multiple_of(2) {
        return Err(Error::new(13, "Odd UTF-16 property size"));
    }
    let units: Vec<u16> = bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|b| u16::from_le_bytes([b[0], b[1]]))
        .collect();
    Ok(units
        .split(|u| *u == 0)
        .take_while(|s| !s.is_empty())
        .map(String::from_utf16_lossy)
        .collect())
}
struct DeviceSet(HDEVINFO);
impl DeviceSet {
    fn new(guid: &GUID, flags: SETUP_DI_GET_CLASS_DEVS_FLAGS) -> Result<Self> {
        // SAFETY: class/interface GUID is valid; the returned device-info set is owned by this guard.
        Ok(Self(unsafe {
            SetupDiGetClassDevsW(Some(guid), PCWSTR::null(), None, flags)?
        }))
    }
    fn property(
        &self,
        info: &SP_DEVINFO_DATA,
        property: SETUP_DI_REGISTRY_PROPERTY,
        expected: u32,
    ) -> Result<Vec<u8>> {
        let mut needed = 0;
        let mut kind = 0;
        // SAFETY: no output storage in initial length query; info belongs to this set.
        let first = unsafe {
            SetupDiGetDeviceRegistryPropertyW(
                self.0,
                info,
                property,
                Some(&mut kind),
                None,
                Some(&mut needed),
            )
        };
        if first.is_err() {
            // SAFETY: capture the preceding API failure immediately.
            let error = unsafe { GetLastError() };
            if error == ERROR_INVALID_DATA || error == ERROR_FILE_NOT_FOUND {
                return Ok(Vec::new());
            }
            if error != ERROR_INSUFFICIENT_BUFFER {
                return Err(Error::new(error.0, "Read device property size"));
            }
        }
        for _ in 0..3 {
            if needed as usize > MAX_PROPERTY || needed % 2 != 0 {
                return Err(Error::new(13, "Invalid device property size"));
            }
            let mut bytes = vec![0; needed as usize];
            // SAFETY: buffer slice bounds are passed by the generated binding.
            if unsafe {
                SetupDiGetDeviceRegistryPropertyW(
                    self.0,
                    info,
                    property,
                    Some(&mut kind),
                    Some(&mut bytes),
                    Some(&mut needed),
                )
            }
            .is_ok()
            {
                if kind != expected || needed as usize > bytes.len() {
                    return Err(Error::new(13, "Device property type/length changed"));
                }
                bytes.truncate(needed as usize);
                return Ok(bytes);
            }
            // SAFETY: capture API error before any other OS work.
            let error = unsafe { GetLastError() };
            if error == ERROR_INVALID_DATA || error == ERROR_FILE_NOT_FOUND {
                return Ok(Vec::new());
            }
            if error != ERROR_INSUFFICIENT_BUFFER {
                return Err(Error::new(error.0, "Read device property"));
            }
        }
        Err(Error::new(1237, "Device property kept changing"))
    }
    fn id(&self, info: &SP_DEVINFO_DATA) -> Result<String> {
        let mut id = [0; 200];
        // SAFETY: info belongs to set and binding bounds the output UTF-16 slice.
        unsafe {
            SetupDiGetDeviceInstanceIdW(self.0, info, Some(&mut id), None)?;
        }
        Ok(text(&id))
    }
    fn eligible(&self, info: &SP_DEVINFO_DATA) -> Result<bool> {
        Ok(info.ClassGuid == KEYBOARD
            && strings(&self.property(info, SPDRP_ENUMERATOR_NAME, REG_SZ.0)?)?
                .first()
                .is_some_and(|value| value.eq_ignore_ascii_case("HID")))
    }
    fn filters(&self, info: &SP_DEVINFO_DATA) -> Result<Vec<String>> {
        strings(&self.property(info, SPDRP_LOWERFILTERS, REG_MULTI_SZ.0)?)
    }
}
impl Drop for DeviceSet {
    fn drop(&mut self) {
        // SAFETY: owned SetupAPI set uses its specific destructor.
        unsafe {
            let _ = SetupDiDestroyDeviceInfoList(self.0);
        }
    }
}
#[derive(Clone, Debug)]
pub struct Keyboard {
    pub instance: String,
    pub mounted: bool,
}
#[derive(Clone, Debug)]
pub struct EndpointInfo {
    pub instance: String,
    pub path: String,
}
pub fn keyboards(present: bool) -> Result<Vec<Keyboard>> {
    let set = DeviceSet::new(
        &KEYBOARD,
        if present {
            DIGCF_PRESENT
        } else {
            SETUP_DI_GET_CLASS_DEVS_FLAGS(0)
        },
    )?;
    let mut result = Vec::new();
    for index in 0.. {
        let mut info = SP_DEVINFO_DATA {
            cbSize: size_of::<SP_DEVINFO_DATA>() as u32,
            ..Default::default()
        };
        // SAFETY: info has correct cbSize and set remains live.
        if unsafe { SetupDiEnumDeviceInfo(set.0, index, &mut info) }.is_err() {
            // SAFETY: capture enum error immediately.
            if unsafe { GetLastError() } == ERROR_NO_MORE_ITEMS {
                break;
            }
            return Err(Error::last("Enumerate HID keyboards"));
        }
        if set.eligible(&info)? {
            result.push(Keyboard {
                instance: set.id(&info)?,
                mounted: set
                    .filters(&info)?
                    .iter()
                    .any(|f| f.eq_ignore_ascii_case(FILTER)),
            });
        }
    }
    Ok(result)
}
pub fn endpoints() -> Result<Vec<EndpointInfo>> {
    let set = DeviceSet::new(&HID_INTERFACE, DIGCF_PRESENT | DIGCF_DEVICEINTERFACE)?;
    let mut result = Vec::new();
    for index in 0.. {
        let mut interface = SP_DEVICE_INTERFACE_DATA {
            cbSize: size_of::<SP_DEVICE_INTERFACE_DATA>() as u32,
            ..Default::default()
        };
        // SAFETY: initialized output and valid set/interface GUID.
        if unsafe {
            SetupDiEnumDeviceInterfaces(set.0, None, &HID_INTERFACE, index, &mut interface)
        }
        .is_err()
        {
            // SAFETY: capture enum error immediately.
            if unsafe { GetLastError() } == ERROR_NO_MORE_ITEMS {
                break;
            }
            return Err(Error::last("Enumerate HID endpoints"));
        }
        let mut needed = 0;
        // SAFETY: query variable-sized interface detail buffer length.
        let _ = unsafe {
            SetupDiGetDeviceInterfaceDetailW(set.0, &interface, None, 0, Some(&mut needed), None)
        };
        if needed < size_of::<SP_DEVICE_INTERFACE_DETAIL_DATA_W>() as u32
            || needed as usize > MAX_PROPERTY
        {
            return Err(Error::new(13, "Invalid interface detail size"));
        }
        let mut storage = vec![0_u64; (needed as usize).div_ceil(8) + 1];
        let mut info = SP_DEVINFO_DATA {
            cbSize: size_of::<SP_DEVINFO_DATA>() as u32,
            ..Default::default()
        };
        // SAFETY: u64 storage aligns the detail structure, has at least needed bytes and zeroed trailing UTF-16 terminator.
        let path = unsafe {
            let detail = storage
                .as_mut_ptr()
                .cast::<SP_DEVICE_INTERFACE_DETAIL_DATA_W>();
            (*detail).cbSize = size_of::<SP_DEVICE_INTERFACE_DETAIL_DATA_W>() as u32;
            SetupDiGetDeviceInterfaceDetailW(
                set.0,
                &interface,
                Some(detail),
                needed,
                None,
                Some(&mut info),
            )?;
            let offset = std::mem::offset_of!(SP_DEVICE_INTERFACE_DETAIL_DATA_W, DevicePath);
            text(std::slice::from_raw_parts(
                storage.as_ptr().cast::<u8>().add(offset).cast::<u16>(),
                (needed as usize - offset) / 2,
            ))
        };
        let mut instance = set.id(&info)?;
        let mut parent = 0;
        let mut id = [0; 200];
        // SAFETY: ConfigMgr writes only to valid local output buffers.
        if unsafe {
            CM_Get_Parent(&mut parent, info.DevInst, 0) == CR_SUCCESS
                && CM_Get_Device_IDW(parent, &mut id, 0) == CR_SUCCESS
        } {
            let parent_id = text(&id);
            if is_rc003(&parent_id) {
                instance = parent_id;
            }
        }
        result.push(EndpointInfo { instance, path });
    }
    Ok(result)
}
fn reject_class_filter() -> Result<()> {
    for property in [0x12, 0x11] {
        let mut needed = 0;
        let mut kind = 0;
        // SAFETY: class property size query with empty output buffer.
        let result = unsafe {
            SetupDiGetClassRegistryPropertyW(
                &KEYBOARD,
                property,
                Some(&mut kind),
                &mut [],
                Some(&mut needed),
                PCWSTR::null(),
                None,
            )
        };
        if result.is_err() {
            // SAFETY: capture error immediately.
            let error = unsafe { GetLastError() };
            if error == ERROR_INVALID_DATA || error == ERROR_FILE_NOT_FOUND {
                continue;
            }
            if error != ERROR_INSUFFICIENT_BUFFER {
                return Err(Error::new(error.0, "Read keyboard class filters"));
            }
        }
        if needed as usize > MAX_PROPERTY || needed % 2 != 0 {
            return Err(Error::new(13, "Invalid class property size"));
        }
        let mut bytes = vec![0; needed as usize];
        // SAFETY: binding bounds initialized byte buffer; class GUID is static.
        unsafe {
            SetupDiGetClassRegistryPropertyW(
                &KEYBOARD,
                property,
                Some(&mut kind),
                &mut bytes,
                Some(&mut needed),
                PCWSTR::null(),
                None,
            )?;
        }
        if kind != REG_MULTI_SZ.0
            || strings(&bytes)?
                .iter()
                .any(|f| f.eq_ignore_ascii_case(FILTER))
        {
            return Err(Error::new(
                5023,
                "Remove legacy keyboard class filter with driver installer, then reboot",
            ));
        }
    }
    Ok(())
}
fn next_filters(previous: &[String], enabled: bool) -> Vec<String> {
    let mut found = false;
    let mut next = Vec::with_capacity(previous.len() + 1);
    for item in previous {
        if !item.eq_ignore_ascii_case(FILTER) {
            next.push(item.clone());
        } else if enabled && !found {
            next.push(item.clone());
            found = true;
        }
    }
    if enabled && !found {
        next.push(FILTER.into());
    }
    next
}
pub fn attachment(instance: &str, enabled: bool) -> Result<()> {
    if instance.is_empty() || instance.encode_utf16().count() >= 200 || instance.contains('\0') {
        return Err(Error::new(87, "Invalid device instance ID"));
    }
    let set = DeviceSet::new(&KEYBOARD, SETUP_DI_GET_CLASS_DEVS_FLAGS(0))?;
    let id = wide(instance);
    let mut info = SP_DEVINFO_DATA {
        cbSize: size_of::<SP_DEVINFO_DATA>() as u32,
        ..Default::default()
    };
    // SAFETY: UTF-16 input and initialized device-info structure live across the call.
    unsafe {
        SetupDiOpenDeviceInfoW(set.0, PCWSTR(id.as_ptr()), None, 0, Some(&mut info))?;
    }
    if !set.eligible(&info)? {
        return Err(Error::new(
            50,
            "Only HID Keyboard collections may be attached",
        ));
    }
    reject_class_filter()?;
    if enabled {
        super::scm::validate_filter()?;
    }
    let previous = set.filters(&info)?;
    let next = next_filters(&previous, enabled);
    if previous != next {
        let mut units = Vec::new();
        for filter in &next {
            units.extend(wide(filter));
        }
        if !units.is_empty() {
            units.push(0);
        }
        let bytes: Vec<u8> = units.into_iter().flat_map(u16::to_le_bytes).collect();
        // SAFETY: buffers are encoded REG_MULTI_SZ, including terminators. None deletes the property when no filters remain.
        unsafe {
            SetupDiSetDeviceRegistryPropertyW(
                set.0,
                &mut info,
                SPDRP_LOWERFILTERS,
                (!bytes.is_empty()).then_some(bytes.as_slice()),
            )?;
        }
        if set.filters(&info)? != next {
            return Err(Error::new(
                1237,
                "Device filters changed concurrently; refresh and retry",
            ));
        }
    }
    let mut node = 0;
    // SAFETY: valid mutable output; ConfigMgr reads the null-terminated instance ID.
    let present =
        unsafe { CM_Locate_DevNodeW(&mut node, PCWSTR(id.as_ptr()), CM_LOCATE_DEVNODE_NORMAL) };
    if present == CR_NO_SUCH_DEVNODE {
        return Ok(());
    }
    if present != CR_SUCCESS {
        return Err(Error::new(present.0, "Locate device before restart"));
    }
    let change = SP_PROPCHANGE_PARAMS {
        ClassInstallHeader: SP_CLASSINSTALL_HEADER {
            cbSize: size_of::<SP_CLASSINSTALL_HEADER>() as u32,
            InstallFunction: DIF_PROPERTYCHANGE,
        },
        StateChange: DICS_PROPCHANGE,
        Scope: DICS_FLAG_CONFIGSPECIFIC,
        HwProfile: 0,
    };
    // SAFETY: params embed the required header at offset zero; both info and set remain valid.
    unsafe {
        SetupDiSetClassInstallParamsW(
            set.0,
            Some(&info),
            Some(&change.ClassInstallHeader),
            size_of::<SP_PROPCHANGE_PARAMS>() as u32,
        )?;
        SetupDiCallClassInstaller(DIF_PROPERTYCHANGE, set.0, Some(&info))?;
    }
    let mut params = SP_DEVINSTALL_PARAMS_W {
        cbSize: size_of::<SP_DEVINSTALL_PARAMS_W>() as u32,
        ..Default::default()
    };
    // SAFETY: initialized correctly sized output for selected device.
    unsafe {
        SetupDiGetDeviceInstallParamsW(set.0, Some(&info), &mut params)?;
    }
    if (params.Flags & (DI_NEEDREBOOT | DI_NEEDRESTART)).0 != 0 {
        log::warn!("Device filter change requires reboot: {instance}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn selection_and_filter_preservation() {
        assert!(is_rc003("HID\\VID&012717_PID&32B8"));
        assert!(!is_rc003("HID\\VID_2717&PID_0001"));
        let prior = vec![
            "other".into(),
            FILTER.into(),
            FILTER.to_lowercase(),
            "tail".into(),
        ];
        assert_eq!(next_filters(&prior, true), vec!["other", FILTER, "tail"]);
        assert_eq!(next_filters(&prior, false), vec!["other", "tail"]);
        assert_eq!(next_filters(&["other".into()], true), vec!["other", FILTER]);
        assert!(strings(&[0]).is_err());
    }
}
