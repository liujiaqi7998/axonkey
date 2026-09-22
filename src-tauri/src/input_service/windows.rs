//! Windows input backend. The desktop process consumes complete HID reports
//! from AxonkeyService over RPC; it does not open Bluetooth, HID, or raw filter
//! device handles.
use super::{
    cursor_delta, repeatable_click, InputServiceStatus, NativeBehavior, NativeSettings,
    RepeatableClick, TriggerBehaviors, WheelDirection,
};
#[cfg(not(test))]
use serde::Serialize;
use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, Mutex, RwLock},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};
#[cfg(not(test))]
use tauri::Emitter;

#[cfg(target_os = "windows")]
type KeyboardEventReceiver = tokio::sync::broadcast::Receiver<crate::service_rpc::KeyboardEvent>;
#[cfg(not(target_os = "windows"))]
type KeyboardEventReceiver = ();

const LONG_PRESS_MS: u64 = 600;
const LONG_PRESS_REPEAT_INITIAL_MS: u64 = 350;
const LONG_PRESS_REPEAT_INTERVAL_MS: u64 = 100;
const DOUBLE_CLICK_MS: u64 = 350;
const OUTPUT_TAP_DURATION: Duration = Duration::from_millis(50);
const REPEAT_INITIAL_MS: u64 = 500;
const REPEAT_INTERVAL_MS: u64 = 50;

#[derive(Clone, Copy)]
struct SourceKey {
    id: &'static str,
    usage: u16,
    original_virtual_key: u16,
}
const SOURCE_KEYS: [SourceKey; 13] = [
    SourceKey {
        id: "voice",
        usage: 0x3e,
        original_virtual_key: 0x74, // F5
    },
    SourceKey {
        id: "power",
        usage: 0x66,
        original_virtual_key: 0x5e, // VK_POWER
    },
    SourceKey {
        id: "home",
        usage: 0x4a,
        original_virtual_key: 0x24, // VK_HOME
    },
    SourceKey {
        id: "tv",
        usage: 0x35,
        original_virtual_key: 0xc0, // VK_OEM_3
    },
    SourceKey {
        id: "menu",
        usage: 0x65,
        original_virtual_key: 0x5d, // VK_APPS
    },
    SourceKey {
        id: "confirm",
        usage: 0x28,
        original_virtual_key: 0x0d, // VK_RETURN
    },
    SourceKey {
        id: "up",
        usage: 0x52,
        original_virtual_key: 0x26, // VK_UP
    },
    SourceKey {
        id: "down",
        usage: 0x51,
        original_virtual_key: 0x28, // VK_DOWN
    },
    SourceKey {
        id: "left",
        usage: 0x50,
        original_virtual_key: 0x25, // VK_LEFT
    },
    SourceKey {
        id: "right",
        usage: 0x4f,
        original_virtual_key: 0x27, // VK_RIGHT
    },
    SourceKey {
        id: "back",
        usage: 0xf1,
        original_virtual_key: 0xa6, // VK_BROWSER_BACK
    },
    SourceKey {
        id: "volumeUp",
        usage: 0x80,
        original_virtual_key: 0xaf, // VK_VOLUME_UP
    },
    SourceKey {
        id: "volumeDown",
        usage: 0x81,
        original_virtual_key: 0xae, // VK_VOLUME_DOWN
    },
];

#[cfg(not(test))]
type EventApp = tauri::AppHandle;
#[cfg(test)]
type EventApp = ();
struct Shared {
    settings: RwLock<NativeSettings>,
    status: Mutex<InputServiceStatus>,
    event_app: RwLock<Option<EventApp>>,
    stop: std::sync::atomic::AtomicBool,
}
pub struct InputService {
    shared: Arc<Shared>,
    worker: Mutex<Option<JoinHandle<()>>>,
}

impl InputService {
    pub fn start() -> Self {
        log::info!(target: "axonkey::input", "Starting Windows RPC input service");
        #[cfg(target_os = "windows")]
        let keyboard_events = crate::service_rpc::subscribe_keyboard_events();
        #[cfg(not(target_os = "windows"))]
        let keyboard_events = ();
        let shared = Arc::new(Shared {
            settings: RwLock::new(NativeSettings::default()),
            status: Mutex::new(InputServiceStatus::default()),
            event_app: RwLock::new(None),
            stop: std::sync::atomic::AtomicBool::new(false),
        });
        let worker_shared = Arc::clone(&shared);
        let worker = thread::Builder::new()
            .name("Axonkey service keyboard input".into())
            .spawn(move || worker_loop(worker_shared, keyboard_events))
            .ok();
        if worker.is_none() {
            shared.status.lock().unwrap().error =
                Some("Cannot start the Windows RPC input worker".into());
        }
        Self {
            shared,
            worker: Mutex::new(worker),
        }
    }
    pub fn update_settings(&self, settings: NativeSettings) -> Result<(), String> {
        validate_settings(&settings)?;
        *self
            .shared
            .settings
            .write()
            .map_err(|_| "Input settings lock is unavailable")? = settings;
        Ok(())
    }
    pub fn shutdown(&self) {
        self.shared
            .stop
            .store(true, std::sync::atomic::Ordering::Relaxed);
        if let Ok(mut worker) = self.worker.lock() {
            if let Some(worker) = worker.take() {
                let _ = worker.join();
            }
        }
    }
    pub fn set_event_app(&self, app: tauri::AppHandle) {
        #[cfg(not(test))]
        if let Ok(mut target) = self.shared.event_app.write() {
            *target = Some(app);
        }
        #[cfg(test)]
        let _ = app;
    }
    pub fn status(&self) -> InputServiceStatus {
        self.shared
            .status
            .lock()
            .map(|s| s.clone())
            .unwrap_or_else(|_| InputServiceStatus {
                error: Some("Input status lock is unavailable".into()),
                ..Default::default()
            })
    }
}
impl Drop for InputService {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn worker_loop(shared: Arc<Shared>, mut stream: KeyboardEventReceiver) {
    #[cfg(target_os = "windows")]
    {
        let mut state = InputState::default();
        let mut current_device: Option<String> = None;
        let mut service_connected = crate::service_rpc::keyboard_connection_active();
        let mut connection_generation = crate::service_rpc::keyboard_connection_generation();
        set_status(
            &shared,
            false,
            None,
            service_connected,
            (!service_connected).then_some("AxonkeyService 键盘事件流不可用".into()),
        );
        while !shared.stop.load(std::sync::atomic::Ordering::Relaxed) {
            let current_generation = crate::service_rpc::keyboard_connection_generation();
            let connection_active = crate::service_rpc::keyboard_connection_active();
            if current_generation != connection_generation || connection_active != service_connected
            {
                connection_generation = current_generation;
                service_connected = connection_active;
                state.release_all_with_events(&shared);
                current_device = None;
                set_status(
                    &shared,
                    false,
                    None,
                    service_connected,
                    (!service_connected).then_some("AxonkeyService 键盘事件流不可用".into()),
                );
            }
            if !service_connected {
                thread::sleep(Duration::from_millis(20));
                continue;
            }
            match stream.try_recv() {
                Ok(event) => {
                    if event.connection_generation < connection_generation {
                        continue;
                    }
                    if event.connection_generation > connection_generation {
                        connection_generation = event.connection_generation;
                        service_connected = crate::service_rpc::keyboard_connection_active();
                        state.release_all_with_events(&shared);
                        current_device = None;
                        set_status(
                            &shared,
                            false,
                            None,
                            service_connected,
                            (!service_connected)
                                .then_some("AxonkeyService 键盘事件流不可用".into()),
                        );
                        if !service_connected {
                            continue;
                        }
                    }
                    if !service_connected {
                        continue;
                    }
                    if event.report.is_empty() {
                        state.release_all_with_events(&shared);
                        current_device = None;
                        set_status(&shared, false, None, true, None);
                        continue;
                    }
                    if current_device.as_deref() != Some(event.device_instance_id.as_str()) {
                        state.release_all_with_events(&shared);
                        current_device = Some(event.device_instance_id.clone());
                    }
                    set_status(&shared, true, Some(event.device_instance_id), true, None);
                    if let Some(usages) = parse_hid_report(&event.report) {
                        state.process_report(&shared, usages);
                    } else {
                        log::warn!(
                            target: "axonkey::input",
                            "Ignoring malformed AxonkeyService keyboard report and releasing active outputs"
                        );
                        state.release_all_with_events(&shared);
                    }
                    state.process_timers(&shared);
                }
                Err(tokio::sync::broadcast::error::TryRecvError::Empty) => {
                    state.process_timers(&shared);
                    thread::sleep(Duration::from_millis(20));
                }
                Err(tokio::sync::broadcast::error::TryRecvError::Lagged(skipped)) => {
                    log::warn!(
                        target: "axonkey::input",
                        "Dropped {skipped} keyboard reports; resetting key state"
                    );
                    state.release_all_with_events(&shared);
                }
                Err(tokio::sync::broadcast::error::TryRecvError::Closed) => {
                    state.release_all_with_events(&shared);
                    current_device = None;
                    service_connected = false;
                    set_status(
                        &shared,
                        false,
                        None,
                        false,
                        Some("AxonkeyService 键盘事件流已关闭".into()),
                    );
                    stream = crate::service_rpc::subscribe_keyboard_events();
                }
            }
        }
        state.release_all_with_events(&shared);
        set_status(&shared, false, None, false, None);
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = (shared, stream);
        thread::park();
    }
}

fn set_status(
    shared: &Shared,
    connected: bool,
    hardware_id: Option<String>,
    ready: bool,
    error: Option<String>,
) {
    if let Ok(mut status) = shared.status.lock() {
        status.backend_ready = ready;
        status.device_connected = connected;
        status.hardware_id = hardware_id;
        status.capture_active = connected;
        status.error = error;
    }
}

fn parse_hid_report(report: &[u8]) -> Option<HashSet<u16>> {
    if !matches!(report.first().copied(), Some(0 | 1)) {
        return None;
    }
    let bytes = &report[1..];
    if !bytes.len().is_multiple_of(2) {
        return None;
    }
    let usages: HashSet<u16> = bytes
        .chunks_exact(2)
        .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
        .filter(|usage| *usage != 0)
        .collect();
    // HID keyboard usages 0x01..0x03 are rollover/error markers, not keys.
    (!usages.iter().any(|usage| (1..=3).contains(usage))).then_some(usages)
}
fn source_for_usage(usage: u16) -> Option<SourceKey> {
    SOURCE_KEYS
        .iter()
        .copied()
        .find(|source| source.usage == usage)
}

#[derive(Default)]
struct InputState {
    active_usages: HashSet<u16>,
    buttons: HashMap<&'static str, ButtonState>,
}
struct PressState {
    started_at: Instant,
    long_fired: bool,
    long_repeat_due_at: Option<Instant>,
    held_outputs: Vec<(u16, bool)>,
    click_repeat: Option<(RepeatableClick, Instant)>,
    next_repeat_at: Instant,
    original_virtual_key: u16,
    passthrough: bool,
}
struct PendingClick {
    due_at: Instant,
    original_virtual_key: u16,
}
#[derive(Default)]
struct ButtonState {
    pressed: Option<PressState>,
    pending_click: Option<PendingClick>,
}

impl InputState {
    fn process_report(&mut self, shared: &Shared, usages: HashSet<u16>) {
        let mut pressed: Vec<_> = usages.difference(&self.active_usages).copied().collect();
        let mut released: Vec<_> = self.active_usages.difference(&usages).copied().collect();
        pressed.sort_unstable();
        released.sort_unstable();
        self.active_usages = usages;
        for usage in pressed {
            if let Some(source) = source_for_usage(usage) {
                emit_remote_key(shared, source.id, true);
                self.press_source(shared, source);
            }
        }
        for usage in released {
            if let Some(source) = source_for_usage(usage) {
                emit_remote_key(shared, source.id, false);
                self.release_source(shared, source);
            }
        }
    }
    fn press_source(&mut self, shared: &Shared, source: SourceKey) {
        let settings = shared
            .settings
            .read()
            .map(|s| s.clone())
            .unwrap_or_default();
        let triggers = settings
            .behaviors
            .get(source.id)
            .cloned()
            .unwrap_or_default();
        let state = self.buttons.entry(source.id).or_default();
        if state.pressed.is_some() {
            return;
        }
        let now = Instant::now();
        if state
            .pending_click
            .as_ref()
            .is_some_and(|p| now >= p.due_at)
        {
            if let Some(pending) = state.pending_click.take() {
                if settings.enabled {
                    execute_click_or_original(&triggers.click, pending.original_virtual_key);
                } else {
                    tap_original_key(pending.original_virtual_key);
                }
            }
        }
        if !settings.enabled || !has_custom_behavior(&triggers) {
            if let Some(pending) = state.pending_click.take() {
                tap_original_key(pending.original_virtual_key);
            }
            send_original_key_down(source.original_virtual_key);
            state.pressed = Some(PressState {
                started_at: now,
                long_fired: false,
                long_repeat_due_at: None,
                held_outputs: Vec::new(),
                click_repeat: None,
                next_repeat_at: now + Duration::from_millis(REPEAT_INITIAL_MS),
                original_virtual_key: source.original_virtual_key,
                passthrough: true,
            });
            return;
        }
        let click_repeat = repeatable_click(&triggers).map(|action| {
            execute_repeatable_click(action);
            (action, now + Duration::from_millis(400))
        });
        let held_outputs = continuous_click_chord(&triggers)
            .map(|keys| press_chord(&keys))
            .unwrap_or_default();
        state.pressed = Some(PressState {
            started_at: now,
            long_fired: false,
            long_repeat_due_at: None,
            held_outputs,
            click_repeat,
            next_repeat_at: now
                + Duration::from_millis(if click_repeat.is_some() {
                    400
                } else {
                    REPEAT_INITIAL_MS
                }),
            original_virtual_key: source.original_virtual_key,
            passthrough: false,
        });
    }
    fn release_source(&mut self, shared: &Shared, source: SourceKey) {
        let settings = shared
            .settings
            .read()
            .map(|s| s.clone())
            .unwrap_or_default();
        let triggers = settings
            .behaviors
            .get(source.id)
            .cloned()
            .unwrap_or_default();
        let Some(state) = self.buttons.get_mut(source.id) else {
            return;
        };
        let Some(press) = state.pressed.take() else {
            return;
        };
        if press.passthrough {
            send_original_key_up(press.original_virtual_key);
            return;
        }
        if press.click_repeat.is_some() {
            return;
        }
        if !press.held_outputs.is_empty() {
            release_chord(&press.held_outputs);
            return;
        }
        if press.long_fired {
            return;
        }
        if has_enabled(&triggers.long_press)
            && press.started_at.elapsed() >= Duration::from_millis(LONG_PRESS_MS)
        {
            execute_behaviors(&triggers.long_press);
            return;
        }
        if press.started_at.elapsed() >= Duration::from_millis(LONG_PRESS_MS) {
            tap_original_key(press.original_virtual_key);
            return;
        }
        if has_enabled(&triggers.double_click) {
            if state.pending_click.take().is_some() {
                execute_behaviors(&triggers.double_click);
            } else {
                state.pending_click = Some(PendingClick {
                    due_at: Instant::now() + Duration::from_millis(DOUBLE_CLICK_MS),
                    original_virtual_key: press.original_virtual_key,
                });
            }
        } else {
            execute_click_or_original(&triggers.click, press.original_virtual_key);
        }
    }
    fn process_timers(&mut self, shared: &Shared) {
        let settings = shared
            .settings
            .read()
            .map(|s| s.clone())
            .unwrap_or_default();
        let now = Instant::now();
        for source in SOURCE_KEYS {
            let Some(state) = self.buttons.get_mut(source.id) else {
                continue;
            };
            let triggers = settings
                .behaviors
                .get(source.id)
                .cloned()
                .unwrap_or_default();
            if !settings.enabled || !has_custom_behavior(&triggers) {
                if let Some(press) = state.pressed.as_mut() {
                    if !press.passthrough {
                        if !press.held_outputs.is_empty() {
                            release_chord(&press.held_outputs);
                            press.held_outputs.clear();
                        }
                        press.click_repeat = None;
                        press.long_repeat_due_at = None;
                        send_original_key_down(press.original_virtual_key);
                        press.passthrough = true;
                        press.next_repeat_at = now + Duration::from_millis(REPEAT_INITIAL_MS);
                    } else if now >= press.next_repeat_at {
                        send_original_key_down(press.original_virtual_key);
                        press.next_repeat_at = now + Duration::from_millis(REPEAT_INTERVAL_MS);
                    }
                }
                if let Some(pending) = state.pending_click.take() {
                    tap_original_key(pending.original_virtual_key);
                }
                continue;
            }
            if let Some(press) = state.pressed.as_mut() {
                if press.passthrough {
                    if now >= press.next_repeat_at {
                        send_original_key_down(press.original_virtual_key);
                        press.next_repeat_at = now + Duration::from_millis(REPEAT_INTERVAL_MS);
                    }
                    continue;
                }
                if let Some((action, due_at)) = press.click_repeat.as_mut() {
                    if repeatable_click(&triggers) == Some(*action) {
                        if now >= *due_at {
                            execute_repeatable_click(*action);
                            *due_at = now + Duration::from_millis(80);
                        }
                        continue;
                    }
                    press.click_repeat = None;
                }
                if now.duration_since(press.started_at) >= Duration::from_millis(LONG_PRESS_MS)
                    && !press.long_fired
                    && press.held_outputs.is_empty()
                {
                    if has_enabled(&triggers.long_press) {
                        execute_behaviors(&triggers.long_press);
                        press.long_fired = true;
                        press.long_repeat_due_at =
                            Some(now + Duration::from_millis(LONG_PRESS_REPEAT_INITIAL_MS));
                    } else {
                        send_original_key_down(press.original_virtual_key);
                        press.passthrough = true;
                        press.next_repeat_at = now + Duration::from_millis(REPEAT_INTERVAL_MS);
                    }
                    state.pending_click = None;
                }
                if let Some(due_at) = press.long_repeat_due_at {
                    if !has_enabled(&triggers.long_press) {
                        press.long_repeat_due_at = None;
                    } else if now >= due_at {
                        execute_behaviors(&triggers.long_press);
                        press.long_repeat_due_at =
                            Some(now + Duration::from_millis(LONG_PRESS_REPEAT_INTERVAL_MS));
                    }
                }
                if now >= press.next_repeat_at {
                    if !press.held_outputs.is_empty() {
                        repeat_chord(&press.held_outputs);
                    }
                    press.next_repeat_at = now + Duration::from_millis(REPEAT_INTERVAL_MS);
                }
            }
            if state.pressed.is_none()
                && state
                    .pending_click
                    .as_ref()
                    .is_some_and(|p| now >= p.due_at)
            {
                if let Some(pending) = state.pending_click.take() {
                    execute_click_or_original(&triggers.click, pending.original_virtual_key);
                }
            }
        }
    }
    fn release_all(&mut self) {
        for state in self.buttons.values_mut() {
            state.pending_click = None;
            if let Some(press) = state.pressed.take() {
                if !press.held_outputs.is_empty() {
                    release_chord(&press.held_outputs);
                }
                if press.passthrough {
                    send_original_key_up(press.original_virtual_key);
                }
            }
        }
        self.active_usages.clear();
    }

    fn release_all_with_events(&mut self, shared: &Shared) {
        let active_usages = self.active_usages.clone();
        self.release_all();
        for usage in active_usages {
            if let Some(source) = source_for_usage(usage) {
                emit_remote_key(shared, source.id, false);
            }
        }
    }
}

#[cfg(not(test))]
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct RemoteKeyEvent {
    button: &'static str,
    pressed: bool,
}
fn emit_remote_key(shared: &Shared, button: &'static str, pressed: bool) {
    let app = shared.event_app.read().ok().and_then(|value| value.clone());
    #[cfg(not(test))]
    if let Some(app) = app {
        let _ = app.emit("axonkey-remote-key", RemoteKeyEvent { button, pressed });
    }
    #[cfg(test)]
    let _ = (app, button, pressed);
}
fn has_enabled(values: &[NativeBehavior]) -> bool {
    values.iter().any(NativeBehavior::enabled)
}
fn has_custom_behavior(triggers: &TriggerBehaviors) -> bool {
    has_enabled(&triggers.click)
        || has_enabled(&triggers.double_click)
        || has_enabled(&triggers.long_press)
}
fn continuous_click_chord(triggers: &TriggerBehaviors) -> Option<Vec<u16>> {
    if has_enabled(&triggers.double_click) || has_enabled(&triggers.long_press) {
        return None;
    }
    let mut values = triggers.click.iter().filter(|v| v.enabled());
    let first = values.next()?;
    if values.next().is_some() {
        return None;
    }
    behavior_chord(first)
}
fn continuous_click_wheel(triggers: &TriggerBehaviors) -> Option<(i32, bool)> {
    match repeatable_click(triggers) {
        Some(RepeatableClick::Wheel(direction)) => Some(wheel_delta(direction)),
        _ => None,
    }
}
fn execute_repeatable_click(action: RepeatableClick) {
    match action {
        RepeatableClick::Wheel(direction) => {
            let (delta, horizontal) = wheel_delta(direction);
            send_wheel_with_axis(delta, horizontal)
        }
        RepeatableClick::CursorMove {
            direction,
            distance,
        } => {
            let (dx, dy) = cursor_delta(direction, distance);
            send_mouse_move(dx, dy);
        }
    }
}
fn wheel_delta(direction: WheelDirection) -> (i32, bool) {
    match direction {
        WheelDirection::Up => (120, false),
        WheelDirection::Down => (-120, false),
        WheelDirection::Left => (-120, true),
        WheelDirection::Right => (120, true),
    }
}
fn execute_click_or_original(values: &[NativeBehavior], original_virtual_key: u16) {
    if has_enabled(values) {
        execute_behaviors(values);
    } else {
        tap_original_key(original_virtual_key);
    }
}
fn execute_behaviors(values: &[NativeBehavior]) {
    for value in values.iter().filter(|v| v.enabled()) {
        match value {
            NativeBehavior::Key { .. } | NativeBehavior::Shortcut { .. } => {
                if let Some(keys) = behavior_chord(value) {
                    tap_chord(&keys);
                }
            }
            NativeBehavior::Wheel { direction, .. } => {
                let (delta, horizontal) = wheel_delta(*direction);
                send_wheel_with_axis(delta, horizontal);
            }
            NativeBehavior::CursorMove {
                direction,
                distance,
                ..
            } => {
                let (dx, dy) = cursor_delta(*direction, *distance);
                send_mouse_move(dx, dy);
            }
            NativeBehavior::Paste { text, .. } => send_unicode_text(text),
            NativeBehavior::Delay { ms, .. } => {
                thread::sleep(Duration::from_millis((*ms).min(300_000)))
            }
            NativeBehavior::Mouse { button, .. } => send_mouse_click(*button),
            NativeBehavior::Disabled { .. } => {}
        }
    }
}
/// System mouse mappings use SendInput, independently of any RC003 keyboard.
pub(super) fn execute_mouse_behavior(behavior: &NativeBehavior, hold_ms: u64) {
    match behavior {
        NativeBehavior::Wheel { direction, .. } => {
            let (delta, horizontal) = wheel_delta(*direction);
            send_wheel_with_axis(delta, horizontal)
        }
        NativeBehavior::CursorMove {
            direction,
            distance,
            ..
        } => {
            let (dx, dy) = cursor_delta(*direction, *distance);
            send_mouse_move(dx, dy);
        }
        NativeBehavior::Mouse { button, .. } => send_mouse_click(*button),
        NativeBehavior::Paste { text, .. } => send_unicode_text(text),
        NativeBehavior::Key { .. } | NativeBehavior::Shortcut { .. } => {
            if let Some(keys) = behavior_chord(behavior) {
                if !send_mouse_chord_with(&keys, hold_ms, send_mouse_keyboard_inputs) {
                    log::warn!(target: "axonkey::input", "Mouse keyboard injection incomplete; target may have higher privileges");
                }
            }
        }
        NativeBehavior::Delay { .. } | NativeBehavior::Disabled { .. } => {}
    }
}

fn virtual_key_input(key: u16) -> Input {
    let scan = unsafe { MapVirtualKeyW(key as u32, 4) };
    let extended = scan >> 8 == 0xe0 || is_extended_key(key);
    Input {
        kind: 1,
        value: InputValue {
            keyboard: KeyboardInput {
                virtual_key: key,
                scan_code: 0,
                flags: u32::from(extended),
                time: 0,
                extra_info: 0,
            },
        },
    }
}

/// Zero hold submits a complete tap in one batch; a configured hold separates
/// down and up for applications that need time to recognize a pressed key.
fn send_mouse_chord_with(
    keys: &[u16],
    hold_ms: u64,
    mut send: impl FnMut(&[Input]) -> usize,
) -> bool {
    if keys.is_empty() {
        return true;
    }
    let downs: Vec<Input> = keys.iter().copied().map(virtual_key_input).collect();
    let release = |mut input: Input| {
        unsafe {
            input.value.keyboard.flags |= 2;
        }
        input
    };
    if hold_ms > 0 {
        let pressed = send(&downs).min(downs.len());
        if pressed == downs.len() {
            thread::sleep(Duration::from_millis(hold_ms.min(1000)));
        }
        let ups: Vec<Input> = downs[..pressed]
            .iter()
            .copied()
            .rev()
            .map(release)
            .collect();
        let released = if ups.is_empty() {
            0
        } else {
            send(&ups).min(ups.len())
        };
        if released < ups.len() {
            send(&ups[released..]);
        }
        return pressed == downs.len() && released == ups.len();
    }
    let mut inputs = Vec::with_capacity(downs.len() * 2);
    inputs.extend_from_slice(&downs);
    inputs.extend(downs.iter().copied().rev().map(release));
    let sent = send(&inputs).min(inputs.len());
    if sent == inputs.len() {
        return true;
    }
    // A partial batch may have inserted downs without their matching ups.
    // Release only those keys, in reverse order, without replaying the action.
    let held = sent.min(downs.len()) - sent.saturating_sub(downs.len());
    if held > 0 {
        let cleanup: Vec<Input> = downs[..held].iter().copied().rev().map(release).collect();
        send(&cleanup);
    }
    false
}

fn send_mouse_keyboard_inputs(inputs: &[Input]) -> usize {
    #[cfg(not(test))]
    {
        unsafe {
            SendInput(
                inputs.len() as u32,
                inputs.as_ptr(),
                std::mem::size_of::<Input>() as i32,
            ) as usize
        }
    }
    #[cfg(test)]
    {
        MOUSE_KEY_BATCHES.with(|batches| {
            batches.borrow_mut().push(
                inputs
                    .iter()
                    .map(|input| {
                        let key = unsafe { input.value.keyboard };
                        (key.virtual_key, key.flags)
                    })
                    .collect(),
            )
        });
        inputs.len()
    }
}

fn behavior_chord(behavior: &NativeBehavior) -> Option<Vec<u16>> {
    match behavior {
        NativeBehavior::Key { key, .. } => parse_chord(key),
        NativeBehavior::Shortcut { keys, .. } => {
            let chord = keys
                .iter()
                .filter_map(|key| parse_chord(key))
                .flatten()
                .fold(Vec::new(), |mut result, key| {
                    if !result.contains(&key) {
                        result.push(key);
                    }
                    result
                });
            (!chord.is_empty()).then_some(chord)
        }
        NativeBehavior::Wheel { .. }
        | NativeBehavior::CursorMove { .. }
        | NativeBehavior::Mouse { .. }
        | NativeBehavior::Paste { .. }
        | NativeBehavior::Delay { .. }
        | NativeBehavior::Disabled { .. } => None,
    }
}

fn press_chord(keys: &[u16]) -> Vec<(u16, bool)> {
    if keys.is_empty() {
        return Vec::new();
    }
    let inputs: Vec<Input> = keys.iter().copied().map(virtual_key_input).collect();
    let sent = send_mouse_keyboard_inputs(&inputs).min(inputs.len());
    let accepted = keys[..sent]
        .iter()
        .copied()
        .map(|key| (key, is_extended_key(key)))
        .collect();
    if sent < inputs.len() {
        let cleanup: Vec<Input> = inputs[..sent]
            .iter()
            .copied()
            .rev()
            .map(keyboard_release)
            .collect();
        let _ = send_mouse_keyboard_inputs(&cleanup);
    }
    accepted
}

fn release_chord(pressed: &[(u16, bool)]) {
    if pressed.is_empty() {
        return;
    }
    let inputs: Vec<Input> = pressed
        .iter()
        .rev()
        .map(|(key, _)| keyboard_release(virtual_key_input(*key)))
        .collect();
    let sent = send_mouse_keyboard_inputs(&inputs).min(inputs.len());
    if sent < inputs.len() {
        let _ = send_mouse_keyboard_inputs(&inputs[sent..]);
    }
}

fn repeat_chord(pressed: &[(u16, bool)]) {
    if let Some((key, _)) = pressed.iter().rev().find(|(key, _)| !is_modifier(*key)) {
        let _ = send_mouse_keyboard_inputs(&[virtual_key_input(*key)]);
    }
}

fn tap_chord(keys: &[u16]) {
    if !send_mouse_chord_with(
        keys,
        OUTPUT_TAP_DURATION.as_millis() as u64,
        send_mouse_keyboard_inputs,
    ) {
        log::warn!(target: "axonkey::input", "Mapped keyboard injection incomplete");
    }
}

fn send_original_key_down(virtual_key: u16) {
    let _ = send_mouse_keyboard_inputs(&[virtual_key_input(virtual_key)]);
}

fn send_original_key_up(virtual_key: u16) {
    let _ = send_mouse_keyboard_inputs(&[keyboard_release(virtual_key_input(virtual_key))]);
}

fn tap_original_key(virtual_key: u16) {
    send_original_key_down(virtual_key);
    thread::sleep(OUTPUT_TAP_DURATION);
    send_original_key_up(virtual_key);
}

fn keyboard_release(mut input: Input) -> Input {
    unsafe {
        input.value.keyboard.flags |= 2;
    }
    input
}

fn is_modifier(vk: u16) -> bool {
    matches!(vk, 0x10 | 0x11 | 0x12 | 0x5b | 0x5c | 0xa0..=0xa5)
}

fn is_extended_key(key: u16) -> bool {
    matches!(key,
        0x5e | 0xa3 | 0xa5 | 0x5b | 0x5c | 0x21..=0x28 | 0x2d | 0x2e | 0x5d | 0xa6..=0xaf | 0xb3
    )
}

fn parse_chord(value: &str) -> Option<Vec<u16>> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return None;
    }
    let mut parts = trimmed
        .split('+')
        .filter(|part| !part.trim().is_empty())
        .collect::<Vec<_>>();
    if trimmed == "+" || trimmed.ends_with("++") {
        parts.push("+");
    }
    let mut keys = Vec::new();
    for part in parts {
        let key = virtual_key_for_name(part.trim())?;
        if !keys.contains(&key) {
            keys.push(key);
        }
    }
    (!keys.is_empty()).then_some(keys)
}

fn virtual_key_for_name(value: &str) -> Option<u16> {
    let upper = value.to_ascii_uppercase();
    let named = match upper.as_str() {
        "CTRL" | "CONTROL" => 0x11,
        "LCTRL" => 0xa2,
        "RCTRL" => 0xa3,
        "SHIFT" => 0x10,
        "LSHIFT" => 0xa0,
        "RSHIFT" => 0xa1,
        "ALT" => 0x12,
        "LALT" => 0xa4,
        "RALT" => 0xa5,
        "WIN" | "LWIN" => 0x5b,
        "RWIN" => 0x5c,
        "ESC" | "ESCAPE" => 0x1b,
        "ENTER" | "RETURN" => 0x0d,
        "SPACE" => 0x20,
        "TAB" => 0x09,
        "BACKSPACE" => 0x08,
        "DELETE" => 0x2e,
        "INSERT" => 0x2d,
        "HOME" => 0x24,
        "END" => 0x23,
        "PAGEUP" => 0x21,
        "PAGEDOWN" => 0x22,
        "UP" | "ARROWUP" => 0x26,
        "DOWN" | "ARROWDOWN" => 0x28,
        "LEFT" | "ARROWLEFT" => 0x25,
        "RIGHT" | "ARROWRIGHT" => 0x27,
        "VOLUMEMUTE" => 0xad,
        "VOLUMEDOWN" => 0xae,
        "VOLUMEUP" => 0xaf,
        "MEDIAPLAYPAUSE" => 0xb3,
        ";" | ":" => 0xba,
        "=" | "+" => 0xbb,
        "," | "，" | "<" => 0xbc,
        "-" | "_" => 0xbd,
        "." | "。" | ">" => 0xbe,
        "/" | "?" | "？" => 0xbf,
        "`" | "~" => 0xc0,
        "[" | "{" | "【" => 0xdb,
        "\\" | "|" => 0xdc,
        "]" | "}" | "】" => 0xdd,
        "'" | "\"" => 0xde,
        _ => 0,
    };
    if named != 0 {
        return Some(named);
    }
    if upper.len() == 1 {
        let byte = upper.as_bytes()[0];
        if byte.is_ascii_alphanumeric() {
            return Some(byte as u16);
        }
    }
    if let Some(number) = upper
        .strip_prefix('F')
        .and_then(|number| number.parse::<u16>().ok())
    {
        if (1..=24).contains(&number) {
            return Some(0x6f + number);
        }
    }
    None
}

fn validate_settings(settings: &NativeSettings) -> Result<(), String> {
    for (button, triggers) in &settings.behaviors {
        for behavior in triggers
            .click
            .iter()
            .chain(&triggers.double_click)
            .chain(&triggers.long_press)
        {
            if !behavior.enabled() {
                continue;
            }
            match behavior {
                NativeBehavior::Key { key, .. } if parse_chord(key).is_none() => {
                    return Err(format!("{button}: unsupported key '{key}'"));
                }
                NativeBehavior::Shortcut { keys, .. }
                    if keys.is_empty() || keys.iter().any(|key| parse_chord(key).is_none()) =>
                {
                    return Err(format!("{button}: shortcut contains an unsupported key"));
                }
                _ => {}
            }
        }
    }
    Ok(())
}

#[repr(C)]
#[derive(Clone, Copy)]
struct MouseInput {
    dx: i32,
    dy: i32,
    mouse_data: u32,
    flags: u32,
    time: u32,
    extra_info: usize,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct KeyboardInput {
    virtual_key: u16,
    scan_code: u16,
    flags: u32,
    time: u32,
    extra_info: usize,
}

#[repr(C)]
#[derive(Clone, Copy)]
union InputValue {
    mouse: MouseInput,
    keyboard: KeyboardInput,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct Input {
    kind: u32,
    value: InputValue,
}

fn wheel_input(delta: i32, horizontal: bool) -> Input {
    Input {
        kind: 0, // INPUT_MOUSE
        value: InputValue {
            mouse: MouseInput {
                dx: 0,
                dy: 0,
                mouse_data: delta as u32,
                flags: if horizontal { 0x1000 } else { 0x0800 },
                time: 0,
                extra_info: 0,
            },
        },
    }
}

#[cfg(test)]
thread_local! {
    static MOUSE_KEY_BATCHES: std::cell::RefCell<Vec<Vec<(u16, u32)>>> = const { std::cell::RefCell::new(Vec::new()) };
    static WHEEL_EVENTS: std::cell::RefCell<Vec<i32>> = const { std::cell::RefCell::new(Vec::new()) };
    static MOUSE_MOVES: std::cell::RefCell<Vec<(i32, i32)>> = const { std::cell::RefCell::new(Vec::new()) };
}

#[cfg(not(test))]
#[repr(C)]
struct WinPoint {
    x: i32,
    y: i32,
}

fn send_mouse_move(dx: i32, dy: i32) {
    #[cfg(test)]
    {
        let _ = (dx, dy);
        MOUSE_MOVES.with(|events| events.borrow_mut().push((dx, dy)));
    }
    #[cfg(not(test))]
    {
        let mut point = WinPoint { x: 0, y: 0 };
        if unsafe { GetCursorPos(&mut point) } == 0 {
            log::warn!(target: "axonkey::input", "Cursor position unavailable: {}", std::io::Error::last_os_error());
            return;
        }
        let vx = unsafe { GetSystemMetrics(76) };
        let vy = unsafe { GetSystemMetrics(77) };
        let vw = unsafe { GetSystemMetrics(78) }.max(1);
        let vh = unsafe { GetSystemMetrics(79) }.max(1);
        let x = (point.x + dx).clamp(vx, vx + vw - 1);
        let y = (point.y + dy).clamp(vy, vy + vh - 1);
        let range_x = (vw - 1).max(1) as i64;
        let range_y = (vh - 1).max(1) as i64;
        let abs_x = ((x - vx) as i64 * 65535 / range_x) as i32;
        let abs_y = ((y - vy) as i64 * 65535 / range_y) as i32;
        const MOUSEEVENTF_MOVE: u32 = 0x0001;
        const MOUSEEVENTF_ABSOLUTE: u32 = 0x8000;
        const MOUSEEVENTF_VIRTUALDESK: u32 = 0x4000;
        let input = Input {
            kind: 0,
            value: InputValue {
                mouse: MouseInput {
                    dx: abs_x,
                    dy: abs_y,
                    mouse_data: 0,
                    flags: MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK,
                    time: 0,
                    extra_info: 0,
                },
            },
        };
        let sent = unsafe { SendInput(1, &input, std::mem::size_of::<Input>() as i32) };
        if sent != 1 {
            log::warn!(target: "axonkey::input", "Cursor move injection failed: dx={dx}, dy={dy}, error={}", std::io::Error::last_os_error());
        }
    }
}

fn send_wheel_with_axis(delta: i32, horizontal: bool) {
    let input = wheel_input(delta, horizontal);
    #[cfg(test)]
    {
        WHEEL_EVENTS.with(|events| {
            events
                .borrow_mut()
                .push(unsafe { input.value.mouse.mouse_data } as i32)
        });
    }
    #[cfg(not(test))]
    {
        let sent = unsafe { SendInput(1, &input, std::mem::size_of::<Input>() as i32) };
        if sent != 1 {
            log::warn!(target: "axonkey::input", "Wheel injection failed: delta={delta}, error={}; target may have higher privileges", std::io::Error::last_os_error());
        }
    }
}

fn send_mouse_click(button: super::MouseButton) {
    let (down, up, mouse_data) = match button {
        super::MouseButton::Left => (0x0002, 0x0004, 0),
        super::MouseButton::Right => (0x0008, 0x0010, 0),
        super::MouseButton::Back => (0x0080, 0x0100, 1),
        super::MouseButton::Forward => (0x0080, 0x0100, 2),
    };
    for flags in [down, up] {
        let input = Input {
            kind: 0,
            value: InputValue {
                mouse: MouseInput {
                    dx: 0,
                    dy: 0,
                    mouse_data: mouse_data << 16,
                    flags,
                    time: 0,
                    extra_info: 0,
                },
            },
        };
        #[cfg(not(test))]
        {
            let _ = unsafe { SendInput(1, &input, std::mem::size_of::<Input>() as i32) };
        }
        #[cfg(test)]
        let _ = input;
    }
}

fn send_unicode_text(text: &str) {
    const INPUT_KEYBOARD: u32 = 1;
    const KEYEVENTF_KEYUP: u32 = 0x0002;
    const KEYEVENTF_UNICODE: u32 = 0x0004;
    let mut expected_events = 0;
    let mut sent_events = 0;
    for code_unit in text.encode_utf16() {
        let input = |flags| Input {
            kind: INPUT_KEYBOARD,
            value: InputValue {
                keyboard: KeyboardInput {
                    virtual_key: 0,
                    scan_code: code_unit,
                    flags,
                    time: 0,
                    extra_info: 0,
                },
            },
        };
        let inputs = [
            input(KEYEVENTF_UNICODE),
            input(KEYEVENTF_UNICODE | KEYEVENTF_KEYUP),
        ];
        expected_events += inputs.len() as u32;
        sent_events += unsafe {
            SendInput(
                inputs.len() as u32,
                inputs.as_ptr(),
                std::mem::size_of::<Input>() as i32,
            )
        };
    }
    log::info!(target: "axonkey::input", "Mapped paste output: expected_events={expected_events}, sent_events={sent_events}");
    if sent_events != expected_events {
        log::warn!(target: "axonkey::input", "Mapped paste injection incomplete: expected_events={expected_events}, sent_events={sent_events}");
    }
}

#[cfg(target_os = "windows")]
#[link(name = "user32")]
extern "system" {
    fn MapVirtualKeyW(code: u32, map_type: u32) -> u32;
    fn SendInput(input_count: u32, inputs: *const Input, input_size: i32) -> u32;
    #[cfg(not(test))]
    fn GetCursorPos(point: *mut WinPoint) -> i32;
    #[cfg(not(test))]
    fn GetSystemMetrics(index: i32) -> i32;
}

#[cfg(not(target_os = "windows"))]
#[allow(non_snake_case)]
unsafe fn MapVirtualKeyW(_code: u32, _map_type: u32) -> u32 {
    0
}

#[cfg(not(target_os = "windows"))]
#[allow(non_snake_case)]
unsafe fn SendInput(_input_count: u32, _inputs: *const Input, _input_size: i32) -> u32 {
    0
}

#[cfg(all(not(target_os = "windows"), not(test)))]
#[allow(non_snake_case)]
unsafe fn GetCursorPos(_point: *mut WinPoint) -> i32 {
    0
}

#[cfg(all(not(target_os = "windows"), not(test)))]
#[allow(non_snake_case)]
unsafe fn GetSystemMetrics(_index: i32) -> i32 {
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_shared(settings: NativeSettings) -> Shared {
        Shared {
            settings: RwLock::new(settings),
            status: Mutex::new(InputServiceStatus::default()),
            event_app: RwLock::new(None),
            stop: std::sync::atomic::AtomicBool::new(false),
        }
    }

    #[test]
    fn parses_service_hid_reports_and_rejects_invalid_ids() {
        assert_eq!(
            parse_hid_report(&[1, 0x3e, 0, 0, 0, 0, 0]).unwrap(),
            [0x3e].into()
        );
        assert_eq!(
            parse_hid_report(&[1, 0xf1, 0, 0x80, 0, 0x81, 0]).unwrap(),
            [0xf1, 0x80, 0x81].into()
        );
        assert_eq!(parse_hid_report(&[0, 0x52, 0]).unwrap(), [0x52].into());
        assert_eq!(
            parse_hid_report(&[1, 0, 0, 0, 0, 0, 0]).unwrap(),
            HashSet::new()
        );
        assert!(parse_hid_report(&[]).is_none());
        assert!(parse_hid_report(&[2, 0, 0]).is_none());
        assert!(parse_hid_report(&[1, 0, 0, 0x3e]).is_none());
        assert!(parse_hid_report(&[1, 1, 0, 0, 0]).is_none());
    }

    #[test]
    fn maps_confirm_and_extended_media_usages() {
        assert_eq!(source_for_usage(0x28).unwrap().id, "confirm");
        assert_eq!(source_for_usage(0xf1).unwrap().id, "back");
        assert_eq!(source_for_usage(0x80).unwrap().id, "volumeUp");
        assert_eq!(source_for_usage(0x81).unwrap().id, "volumeDown");
        assert_eq!(source_for_usage(0xf1).unwrap().original_virtual_key, 0xa6);
        assert_eq!(source_for_usage(0x80).unwrap().original_virtual_key, 0xaf);
        assert_eq!(source_for_usage(0x81).unwrap().original_virtual_key, 0xae);
        assert_ne!(
            unsafe { virtual_key_input(0xa6).value.keyboard.flags } & 1,
            0
        );
        assert_ne!(
            unsafe { virtual_key_input(0xaf).value.keyboard.flags } & 1,
            0
        );
    }

    #[test]
    fn unmapped_windows_button_replays_original_virtual_key() {
        MOUSE_KEY_BATCHES.with(|batches| batches.borrow_mut().clear());
        let shared = test_shared(NativeSettings::default());
        let source = source_for_usage(0xf1).unwrap();
        let mut state = InputState::default();

        state.press_source(&shared, source);
        state.release_source(&shared, source);

        MOUSE_KEY_BATCHES.with(|batches| {
            let batches = batches.borrow();
            assert_eq!(batches.len(), 2);
            assert_eq!(batches[0][0].0, 0xa6);
            assert_eq!(batches[0][0].1 & 2, 0);
            assert_eq!(batches[1][0].0, 0xa6);
            assert_eq!(batches[1][0].1 & 2, 2);
        });
    }

    #[test]
    fn configured_windows_button_uses_behavior_output_instead_of_original_key() {
        MOUSE_KEY_BATCHES.with(|batches| batches.borrow_mut().clear());
        let mut settings = NativeSettings::default();
        settings.enabled = true;
        settings.behaviors.insert(
            "back".into(),
            TriggerBehaviors {
                click: vec![NativeBehavior::Key {
                    enabled: true,
                    key: "Escape".into(),
                }],
                ..TriggerBehaviors::default()
            },
        );
        let shared = test_shared(settings);
        let source = source_for_usage(0xf1).unwrap();
        let mut state = InputState::default();

        state.press_source(&shared, source);
        state.release_source(&shared, source);

        MOUSE_KEY_BATCHES.with(|batches| {
            let batches = batches.borrow();
            assert_eq!(batches.len(), 2);
            assert_eq!(batches[0][0].0, 0x1b);
            assert_eq!(batches[1][0].0, 0x1b);
            assert!(batches
                .iter()
                .all(|batch| batch.iter().all(|(key, _)| *key != 0xa6)));
        });
    }

    #[test]
    fn mouse_shortcut_burst_has_no_per_event_hold_delay() {
        MOUSE_KEY_BATCHES.with(|batches| batches.borrow_mut().clear());
        let started = Instant::now();
        for index in 0..40 {
            execute_mouse_behavior(
                &NativeBehavior::Shortcut {
                    enabled: true,
                    keys: if index % 2 == 0 {
                        vec!["Ctrl".into(), "Shift".into(), "Tab".into()]
                    } else {
                        vec!["Ctrl".into(), "Tab".into()]
                    },
                },
                0,
            );
        }
        let elapsed = started.elapsed();
        println!("40 alternating mouse shortcut outputs (mock injection): {elapsed:?}");
        MOUSE_KEY_BATCHES.with(|batches| {
            let batches = batches.borrow();
            assert_eq!(batches.len(), 40);
            for (index, batch) in batches.iter().enumerate() {
                let expected = if index % 2 == 0 {
                    vec![
                        (0x11, 0),
                        (0x10, 0),
                        (0x09, 0),
                        (0x09, 2),
                        (0x10, 2),
                        (0x11, 2),
                    ]
                } else {
                    vec![(0x11, 0), (0x09, 0), (0x09, 2), (0x11, 2)]
                };
                assert_eq!(
                    *batch, expected,
                    "every notch must preserve order and release its modifiers"
                );
            }
        });
        // Generous headroom for CI scheduling; the former fixed 50 ms hold
        // necessarily took at least two seconds, even without OS injection.
        assert!(
            elapsed < Duration::from_secs(1),
            "mouse output is serialized behind a per-event hold: {elapsed:?}"
        );
    }

    #[test]
    fn configured_mouse_hold_separates_down_and_up() {
        let started = Instant::now();
        let mut batches = Vec::new();
        assert!(send_mouse_chord_with(&[0x11, 0x09], 20, |inputs| {
            batches.push((
                started.elapsed(),
                inputs
                    .iter()
                    .map(|input| unsafe {
                        (input.value.keyboard.virtual_key, input.value.keyboard.flags)
                    })
                    .collect::<Vec<_>>(),
            ));
            inputs.len()
        }));
        assert_eq!(batches.len(), 2);
        assert!(batches[1].0 - batches[0].0 >= Duration::from_millis(20));
        assert_eq!(batches[0].1, vec![(0x11, 0), (0x09, 0)]);
        assert_eq!(batches[1].1, vec![(0x09, 2), (0x11, 2)]);
    }

    #[test]
    fn configured_mouse_hold_cleans_up_partial_injection() {
        for partial_release in [false, true] {
            let mut calls = 0;
            let mut held = Vec::new();
            assert!(!send_mouse_chord_with(&[0x11, 0x09], 1, |inputs| {
                calls += 1;
                let sent = if (partial_release && calls == 2) || (!partial_release && calls == 1) {
                    1
                } else {
                    inputs.len()
                };
                for input in &inputs[..sent] {
                    let key = unsafe { input.value.keyboard };
                    if key.flags & 2 == 0 {
                        held.push(key.virtual_key);
                    } else {
                        assert_eq!(held.pop(), Some(key.virtual_key));
                    }
                }
                sent
            }));
            assert!(held.is_empty());
        }
    }

    #[test]
    fn partial_mouse_chord_injection_releases_only_unmatched_downs() {
        let keys = [0x11, 0x10, 0x09];
        for accepted in 0..=6 {
            let mut calls = 0;
            let mut held = Vec::new();
            let complete = send_mouse_chord_with(&keys, 0, |inputs| {
                calls += 1;
                let sent = if calls == 1 { accepted } else { inputs.len() };
                for input in &inputs[..sent] {
                    assert_eq!(input.kind, 1);
                    let key = unsafe { input.value.keyboard };
                    if key.flags & 2 == 0 {
                        held.push(key.virtual_key);
                    } else {
                        assert_eq!(held.pop(), Some(key.virtual_key));
                    }
                }
                sent
            });
            assert_eq!(complete, accepted == 6);
            assert!(
                held.is_empty(),
                "partial injection must not leave Ctrl/Shift held"
            );
            assert_eq!(calls, if accepted == 0 || accepted == 6 { 1 } else { 2 });
        }
    }

    #[test]
    fn wheel_events_use_signed_windows_notches_without_mouse_movement() {
        for delta in [120, -120] {
            let input = wheel_input(delta, false);
            assert_eq!(input.kind, 0);
            let mouse = unsafe { input.value.mouse };
            assert_eq!(mouse.flags, 0x0800);
            assert_eq!(mouse.mouse_data as i32, delta);
            assert_eq!((mouse.dx, mouse.dy), (0, 0));
        }
    }

    #[test]
    fn continuous_wheel_does_not_bypass_other_gestures_or_sequences() {
        let mut triggers: TriggerBehaviors = serde_json::from_value(serde_json::json!({
            "click": [{"type":"wheel", "direction":"down"}]
        }))
        .unwrap();
        assert_eq!(continuous_click_wheel(&triggers), Some((-120, false)));
        assert!(continuous_click_chord(&triggers).is_none());
        let wheel = triggers.click[0].clone();
        triggers.double_click.push(wheel.clone());
        assert_eq!(continuous_click_wheel(&triggers), None);
        triggers.double_click.clear();
        triggers.long_press.push(wheel.clone());
        assert_eq!(continuous_click_wheel(&triggers), None);
        triggers.long_press.clear();
        triggers.click.push(wheel);
        assert_eq!(continuous_click_wheel(&triggers), None);
        assert!(serde_json::from_value::<NativeBehavior>(serde_json::json!({
            "type":"wheel", "direction":"left"
        }))
        .is_ok());
        let mut cursor: TriggerBehaviors = serde_json::from_value(serde_json::json!({
            "click": [{"type":"cursorMove", "direction":"up"}]
        }))
        .unwrap();
        assert_eq!(
            repeatable_click(&cursor),
            Some(RepeatableClick::CursorMove {
                direction: WheelDirection::Up,
                distance: 50,
            })
        );
        cursor.double_click.push(cursor.click[0].clone());
        assert_eq!(repeatable_click(&cursor), None);
    }

    #[test]
    fn cursor_move_injects_relative_delta_from_current_position() {
        MOUSE_MOVES.with(|events| events.borrow_mut().clear());
        execute_mouse_behavior(
            &NativeBehavior::CursorMove {
                enabled: true,
                direction: WheelDirection::Left,
                distance: 20,
            },
            0,
        );
        MOUSE_MOVES.with(|events| assert_eq!(*events.borrow(), vec![(-20, 0)]));
    }
    #[test]
    fn parses_bracket_and_shortcuts() {
        assert_eq!(parse_chord("]"), Some(vec![0xdd]));
        assert_eq!(parse_chord("】"), Some(vec![0xdd]));
        assert_eq!(parse_chord("Ctrl+C"), Some(vec![0x11, 0x43]));
    }

    #[test]
    fn holds_a_single_click_key_when_no_other_gesture_is_configured() {
        let mut triggers = TriggerBehaviors::default();
        triggers.click.push(NativeBehavior::Key {
            enabled: true,
            key: "RAlt".into(),
        });

        assert_eq!(continuous_click_chord(&triggers), Some(vec![0xa5]));

        triggers.long_press.push(NativeBehavior::Key {
            enabled: true,
            key: "Escape".into(),
        });
        assert_eq!(continuous_click_chord(&triggers), None);
    }

    #[test]
    fn keeps_gesture_detection_for_double_clicks_and_action_sequences() {
        let key = NativeBehavior::Key {
            enabled: true,
            key: "RAlt".into(),
        };
        let mut with_double_click = TriggerBehaviors {
            click: vec![key.clone()],
            ..TriggerBehaviors::default()
        };
        with_double_click.double_click.push(NativeBehavior::Key {
            enabled: true,
            key: "Escape".into(),
        });
        assert_eq!(continuous_click_chord(&with_double_click), None);

        let action_sequence = TriggerBehaviors {
            click: vec![
                key,
                NativeBehavior::Delay {
                    enabled: true,
                    ms: 10,
                },
            ],
            ..TriggerBehaviors::default()
        };
        assert_eq!(continuous_click_chord(&action_sequence), None);
    }

    #[test]
    fn disabled_behavior_suppresses_output_without_holding_a_key() {
        let triggers = TriggerBehaviors {
            click: vec![NativeBehavior::Disabled { enabled: true }],
            ..TriggerBehaviors::default()
        };

        assert!(has_custom_behavior(&triggers));
        assert_eq!(continuous_click_chord(&triggers), None);
    }
}
