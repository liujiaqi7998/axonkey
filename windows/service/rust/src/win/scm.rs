use super::handles::{wide, Event};
use crate::{cancel::Cancel, lock, Error, Result};
use std::{
    mem::size_of,
    sync::{Arc, Mutex},
};
use windows::{
    core::{w, PCWSTR, PWSTR},
    Win32::{Foundation::HANDLE, System::Services::*, UI::WindowsAndMessaging::*},
};

pub struct ServiceHandle(pub SC_HANDLE);
impl Drop for ServiceHandle {
    fn drop(&mut self) {
        // SAFETY: owned SCM handle, closed by its documented destructor.
        unsafe {
            let _ = CloseServiceHandle(self.0);
        }
    }
}
pub fn manager() -> Result<ServiceHandle> {
    // SAFETY: opens the local SCM with only connect access.
    Ok(ServiceHandle(unsafe {
        OpenSCManagerW(PCWSTR::null(), PCWSTR::null(), SC_MANAGER_CONNECT)?
    }))
}
pub fn set_autostart(enabled: bool) -> Result<()> {
    let manager = manager()?;
    // SAFETY: the manager is live; service name is a static UTF-16 string.
    let service = ServiceHandle(unsafe {
        OpenServiceW(manager.0, w!("AxonkeyService"), SERVICE_CHANGE_CONFIG)?
    });
    // SAFETY: no string fields change; null optional fields are accepted by SCM.
    unsafe {
        ChangeServiceConfigW(
            service.0,
            ENUM_SERVICE_TYPE(SERVICE_NO_CHANGE),
            if enabled {
                SERVICE_AUTO_START
            } else {
                SERVICE_DEMAND_START
            },
            SERVICE_ERROR(SERVICE_NO_CHANGE),
            PCWSTR::null(),
            PCWSTR::null(),
            None,
            PCWSTR::null(),
            PCWSTR::null(),
            PCWSTR::null(),
            PCWSTR::null(),
        )?;
    }
    Ok(())
}
pub fn validate_filter() -> Result<()> {
    let manager = manager()?;
    // SAFETY: the manager is live and name is static.
    let service = ServiceHandle(unsafe {
        OpenServiceW(
            manager.0,
            w!("QuarborHIDFilterDriver"),
            SERVICE_QUERY_CONFIG,
        )?
    });
    let mut needed = 0;
    // SAFETY: querying required bytes with no output allocation.
    let _ = unsafe { QueryServiceConfigW(service.0, None, 0, &mut needed) };
    if needed < size_of::<QUERY_SERVICE_CONFIGW>() as u32 || needed > 1024 * 1024 {
        return Err(Error::last("Invalid filter service configuration size"));
    }
    let mut storage = vec![0_usize; (needed as usize).div_ceil(size_of::<usize>())];
    // SAFETY: usize allocation provides required pointer alignment and at least needed bytes; config is used before storage drops.
    let config = unsafe {
        let ptr = storage.as_mut_ptr().cast::<QUERY_SERVICE_CONFIGW>();
        QueryServiceConfigW(service.0, Some(ptr), needed, &mut needed)?;
        &*ptr
    };
    if config.dwServiceType != SERVICE_KERNEL_DRIVER || config.dwStartType == SERVICE_DISABLED {
        return Err(Error::new(
            1058,
            "Quarbor filter is not an enabled kernel driver",
        ));
    }
    Ok(())
}

struct Status {
    handle: usize,
    checkpoint: u32,
}
pub struct Context {
    pub stop: Arc<Cancel>,
    pub rescan: Arc<Event>,
    status: Mutex<Status>,
    result: Mutex<Option<Error>>,
}
impl Context {
    pub fn update(&self, state: SERVICE_STATUS_CURRENT_STATE, error: u32) {
        let mut current = lock(&self.status);
        if current.handle == 0 {
            return;
        }
        let state = if state == SERVICE_RUNNING && self.stop.is_requested() {
            SERVICE_STOP_PENDING
        } else {
            state
        };
        let pending = state == SERVICE_START_PENDING || state == SERVICE_STOP_PENDING;
        current.checkpoint = if pending {
            current.checkpoint.saturating_add(1)
        } else {
            0
        };
        let status = SERVICE_STATUS {
            dwServiceType: SERVICE_WIN32_OWN_PROCESS,
            dwCurrentState: state,
            dwControlsAccepted: if state == SERVICE_RUNNING {
                SERVICE_ACCEPT_STOP | SERVICE_ACCEPT_SHUTDOWN
            } else {
                0
            },
            dwWin32ExitCode: error,
            dwServiceSpecificExitCode: 0,
            dwCheckPoint: current.checkpoint,
            dwWaitHint: if pending { 5000 } else { 0 },
        };
        // SAFETY: this non-owning SCM status token remains valid until ServiceMain/dispatcher exit. Mutex serializes callbacks and main.
        let result =
            unsafe { SetServiceStatus(SERVICE_STATUS_HANDLE(current.handle as *mut _), &status) };
        if let Err(e) = result {
            log::warn!("SetServiceStatus failed: {e}");
        }
    }
}
static CONTEXT: Mutex<Option<Arc<Context>>> = Mutex::new(None);
pub fn dispatch() -> Result<()> {
    let context = Arc::new(Context {
        stop: Arc::new(Cancel::default()),
        rescan: Arc::new(Event::new()?),
        status: Mutex::new(Status {
            handle: 0,
            checkpoint: 0,
        }),
        result: Mutex::new(None),
    });
    *lock(&CONTEXT) = Some(context.clone());
    let name = wide("AxonkeyService");
    let table = [
        SERVICE_TABLE_ENTRYW {
            lpServiceName: PWSTR(name.as_ptr().cast_mut()),
            lpServiceProc: Some(service_main),
        },
        SERVICE_TABLE_ENTRYW::default(),
    ];
    // SAFETY: table/name and global Arc outlive the blocking dispatcher and all SCM callbacks.
    let dispatched = unsafe { StartServiceCtrlDispatcherW(table.as_ptr()) }.map_err(Error::from);
    lock(&CONTEXT).take();
    dispatched?;
    let error = lock(&context.result).take();
    error.map_or(Ok(()), Err)
}
unsafe extern "system" fn service_main(_: u32, _: *mut PWSTR) {
    let Some(context) = lock(&CONTEXT).clone() else {
        return;
    };
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
        // SAFETY: the global and local Arcs keep Context alive until dispatcher completion; callback only borrows it.
        let handle = unsafe {
            RegisterServiceCtrlHandlerExW(
                w!("AxonkeyService"),
                Some(control),
                Some(Arc::as_ptr(&context).cast()),
            )?
        };
        lock(&context.status).handle = handle.0 as usize;
        context.update(SERVICE_START_PENDING, 0);
        crate::service::run(&context)
    }));
    let error = match result {
        Ok(Ok(())) => None,
        Ok(Err(e)) => Some(e),
        Err(_) => Some(Error::new(574, "Service worker panicked")),
    };
    if let Some(e) = &error {
        log::error!("AxonkeyService stopped: {e}");
    }
    context.update(SERVICE_STOPPED, error.as_ref().map_or(0, |e| e.native));
    *lock(&context.result) = error;
}
unsafe extern "system" fn control(
    control: u32,
    event: u32,
    _: *mut core::ffi::c_void,
    pointer: *mut core::ffi::c_void,
) -> u32 {
    let result = std::panic::catch_unwind(|| {
        if pointer.is_null() {
            return 87;
        }
        // SAFETY: pointer was registered from Context's Arc, retained throughout dispatcher lifetime.
        let context = unsafe { &*pointer.cast::<Context>() };
        if control == SERVICE_CONTROL_STOP || control == SERVICE_CONTROL_SHUTDOWN {
            context.stop.request();
            context.update(SERVICE_STOP_PENDING, 0);
            context.rescan.set();
        } else if control == SERVICE_CONTROL_DEVICEEVENT
            && matches!(
                event,
                DBT_DEVICEARRIVAL | DBT_DEVNODES_CHANGED | DBT_DEVICEREMOVECOMPLETE
            )
        {
            context.rescan.set();
        }
        0
    });
    result.unwrap_or(574)
}
pub struct Notification(HDEVNOTIFY);
impl Notification {
    pub fn register(context: &Context) -> Result<Self> {
        let filter = DEV_BROADCAST_DEVICEINTERFACE_W {
            dbcc_size: size_of::<DEV_BROADCAST_DEVICEINTERFACE_W>() as u32,
            dbcc_devicetype: DBT_DEVTYP_DEVICEINTERFACE.0,
            dbcc_classguid: windows::core::GUID::from_u128(0x884b96c3_56ef_11d1_bc8c_00a0c91405dd),
            ..Default::default()
        };
        // SAFETY: valid SCM status token and initialized filter; Windows copies the filter.
        Ok(Self(unsafe {
            RegisterDeviceNotificationW(
                HANDLE(lock(&context.status).handle as *mut _),
                (&filter as *const DEV_BROADCAST_DEVICEINTERFACE_W).cast(),
                DEVICE_NOTIFY_SERVICE_HANDLE,
            )?
        }))
    }
}
impl Drop for Notification {
    fn drop(&mut self) {
        // SAFETY: this guard owns the notification registration.
        unsafe {
            let _ = UnregisterDeviceNotification(self.0);
        }
    }
}
