use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use crate::ports::{
    AudioInput, KikuError, MAX_SESSION_DURATION, MIN_HOLD_DURATION, MediaController, PostProcessor,
    STTProvider, StatusNotifier, TextInjector,
};
use crate::session::DictationCoordinator;

/// Supported global press-and-hold Activation Keys.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActivationKey {
    RightOption,
    LeftOption,
    RightControl,
    LeftControl,
    RightCommand,
    LeftCommand,
    Fn,
    F13,
    F14,
    F15,
    F16,
    F17,
    F18,
    F19,
    F20,
}

impl ActivationKey {
    pub fn parse(raw: &str) -> Result<Self, KikuError> {
        let norm: String = raw
            .chars()
            .filter(|c| !c.is_whitespace() && *c != '_' && *c != '-')
            .flat_map(char::to_lowercase)
            .collect();

        match norm.as_str() {
            "rightoption" | "rightalt" | "roption" | "ralt" => Ok(Self::RightOption),
            "leftoption" | "leftalt" | "loption" | "lalt" => Ok(Self::LeftOption),
            "rightcontrol" | "rightctrl" | "rctrl" => Ok(Self::RightControl),
            "leftcontrol" | "leftctrl" | "lctrl" => Ok(Self::LeftControl),
            "rightcommand" | "rightcmd" | "rightsuper" | "rightwin" => Ok(Self::RightCommand),
            "leftcommand" | "leftcmd" | "leftsuper" | "leftwin" => Ok(Self::LeftCommand),
            "fn" | "globe" => Ok(Self::Fn),
            "f13" => Ok(Self::F13),
            "f14" => Ok(Self::F14),
            "f15" => Ok(Self::F15),
            "f16" => Ok(Self::F16),
            "f17" => Ok(Self::F17),
            "f18" => Ok(Self::F18),
            "f19" => Ok(Self::F19),
            "f20" => Ok(Self::F20),
            _ => Err(KikuError::Config(format!(
                "unsupported activation_key '{raw}' (try RightOption, RightAlt, LeftOption, RightControl, Fn, or F13..F20)"
            ))),
        }
    }
}

/// Raw key-edge event emitted by the OS keyboard hook.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyEdge {
    Down,
    Up,
}

/// Action emitted by `HoldKeyTracker` to drive `DictationCoordinator`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HoldAction {
    None,
    StartSession,
    FinishSession(Duration),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HoldState {
    Idle,
    Pending(Instant),
    Active(Instant),
    Capped,
}

/// Debounces key-down (<250ms taps are ignored before touching audio or media)
/// and enforces the 5-minute max session cap.
pub struct HoldKeyTracker {
    state: HoldState,
}

impl Default for HoldKeyTracker {
    fn default() -> Self {
        Self::new()
    }
}

impl HoldKeyTracker {
    pub fn new() -> Self {
        Self {
            state: HoldState::Idle,
        }
    }

    pub fn on_key_down(&mut self, now: Instant) {
        if self.state == HoldState::Idle {
            self.state = HoldState::Pending(now);
        }
    }

    pub fn on_tick(&mut self, now: Instant) -> HoldAction {
        match self.state {
            HoldState::Pending(pressed_at)
                if now.saturating_duration_since(pressed_at) >= MIN_HOLD_DURATION =>
            {
                self.state = HoldState::Active(pressed_at);
                HoldAction::StartSession
            }
            HoldState::Active(pressed_at)
                if now.saturating_duration_since(pressed_at) >= MAX_SESSION_DURATION =>
            {
                self.state = HoldState::Capped;
                HoldAction::FinishSession(MAX_SESSION_DURATION)
            }
            _ => HoldAction::None,
        }
    }

    pub fn on_key_up(&mut self, now: Instant) -> HoldAction {
        match self.state {
            HoldState::Idle => HoldAction::None,
            HoldState::Pending(_) | HoldState::Capped => {
                self.state = HoldState::Idle;
                HoldAction::None
            }
            HoldState::Active(pressed_at) => {
                self.state = HoldState::Idle;
                HoldAction::FinishSession(now.saturating_duration_since(pressed_at))
            }
        }
    }
}

/// Runs the global hold-key event loop until the key-event channel disconnects.
pub fn run_hold_loop<A, S, P, T, M, N>(
    coordinator: &mut DictationCoordinator<A, S, P, T, M, N>,
    events: &Receiver<KeyEdge>,
) where
    A: AudioInput,
    S: STTProvider,
    P: PostProcessor,
    T: TextInjector,
    M: MediaController,
    N: StatusNotifier,
{
    let mut tracker = HoldKeyTracker::new();
    let tick = Duration::from_millis(20);

    loop {
        match events.recv_timeout(tick) {
            Ok(KeyEdge::Down) => tracker.on_key_down(Instant::now()),
            Ok(KeyEdge::Up) => {
                if let HoldAction::FinishSession(dur) = tracker.on_key_up(Instant::now())
                    && let Err(err) = coordinator.finish_session(dur)
                {
                    eprintln!("[kiku] session finish error: {err}");
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }

        match tracker.on_tick(Instant::now()) {
            HoldAction::StartSession => {
                if let Err(err) = coordinator.start_session() {
                    eprintln!("[kiku] session start error: {err}");
                }
            }
            HoldAction::FinishSession(dur) => {
                if let Err(err) = coordinator.finish_session(dur) {
                    eprintln!("[kiku] session cap finish error: {err}");
                }
            }
            HoldAction::None => {}
        }
    }
}

pub fn is_input_monitoring_trusted() -> bool {
    platform::is_input_monitoring_trusted()
}

pub fn spawn_key_listener(key: ActivationKey) -> Result<Receiver<KeyEdge>, KikuError> {
    if !is_input_monitoring_trusted() {
        return Err(KikuError::Config(
            "macOS Input Monitoring permission is required for global key-hold detection. Enable kiku in System Settings -> Privacy & Security -> Input Monitoring.".to_owned(),
        ));
    }
    let (tx, rx) = mpsc::channel();
    platform::spawn_hook(key, tx)?;
    Ok(rx)
}

#[cfg(target_os = "windows")]
mod platform {
    use std::sync::Mutex;
    use std::sync::mpsc::Sender;
    use std::thread;

    use windows::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        VK_F13, VK_F14, VK_F15, VK_F16, VK_F17, VK_F18, VK_F19, VK_F20, VK_LCONTROL, VK_LMENU,
        VK_LWIN, VK_RCONTROL, VK_RMENU, VK_RWIN,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        CallNextHookEx, DispatchMessageW, GetMessageW, HHOOK, KBDLLHOOKSTRUCT, MSG,
        SetWindowsHookExW, TranslateMessage, UnhookWindowsHookEx, WH_KEYBOARD_LL, WM_KEYDOWN,
        WM_KEYUP, WM_SYSKEYDOWN, WM_SYSKEYUP,
    };

    use super::{ActivationKey, KeyEdge};
    use crate::ports::KikuError;

    struct HookState {
        target_vk: u32,
        tx: Sender<KeyEdge>,
    }

    static HOOK_STATE: Mutex<Option<HookState>> = Mutex::new(None);

    pub fn is_input_monitoring_trusted() -> bool {
        true
    }

    fn to_vk(key: ActivationKey) -> Result<u32, KikuError> {
        let vk = match key {
            ActivationKey::RightOption => u32::from(VK_RMENU.0),
            ActivationKey::LeftOption => u32::from(VK_LMENU.0),
            ActivationKey::RightControl => u32::from(VK_RCONTROL.0),
            ActivationKey::LeftControl => u32::from(VK_LCONTROL.0),
            ActivationKey::RightCommand => u32::from(VK_RWIN.0),
            ActivationKey::LeftCommand => u32::from(VK_LWIN.0),
            ActivationKey::F13 => u32::from(VK_F13.0),
            ActivationKey::F14 => u32::from(VK_F14.0),
            ActivationKey::F15 => u32::from(VK_F15.0),
            ActivationKey::F16 => u32::from(VK_F16.0),
            ActivationKey::F17 => u32::from(VK_F17.0),
            ActivationKey::F18 => u32::from(VK_F18.0),
            ActivationKey::F19 => u32::from(VK_F19.0),
            ActivationKey::F20 => u32::from(VK_F20.0),
            ActivationKey::Fn => {
                return Err(KikuError::Config(
                    "Fn key is not exposed by Windows WH_KEYBOARD_LL; choose RightAlt or F13..F20"
                        .to_owned(),
                ));
            }
        };
        Ok(vk)
    }

    unsafe extern "system" fn ll_keyboard_proc(
        code: i32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        if code >= 0 {
            let kb = unsafe { &*(lparam.0 as *const KBDLLHOOKSTRUCT) };
            let msg = wparam.0 as u32;
            if let Ok(guard) = HOOK_STATE.lock()
                && let Some(state) = guard.as_ref()
                && kb.vkCode == state.target_vk
            {
                let edge = match msg {
                    WM_KEYDOWN | WM_SYSKEYDOWN => Some(KeyEdge::Down),
                    WM_KEYUP | WM_SYSKEYUP => Some(KeyEdge::Up),
                    _ => None,
                };
                if let Some(edge) = edge {
                    let _ = state.tx.send(edge);
                }
            }
        }
        unsafe { CallNextHookEx(Some(HHOOK::default()), code, wparam, lparam) }
    }

    pub fn spawn_hook(key: ActivationKey, tx: Sender<KeyEdge>) -> Result<(), KikuError> {
        let target_vk = to_vk(key)?;
        {
            let mut guard = HOOK_STATE
                .lock()
                .map_err(|_| KikuError::Config("keyboard hook state poisoned".to_owned()))?;
            *guard = Some(HookState { target_vk, tx });
        }

        thread::spawn(move || unsafe {
            let hmod = GetModuleHandleW(None).ok().map(Into::into);
            let Ok(hook) = SetWindowsHookExW(WH_KEYBOARD_LL, Some(ll_keyboard_proc), hmod, 0)
            else {
                eprintln!("[kiku] failed to install WH_KEYBOARD_LL hook");
                return;
            };
            let mut msg = MSG::default();
            while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
            let _ = UnhookWindowsHookEx(hook);
        });
        Ok(())
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use std::ffi::c_void;
    use std::sync::mpsc::Sender;
    use std::thread;

    use super::{ActivationKey, KeyEdge};
    use crate::ports::KikuError;

    #[link(name = "CoreGraphics", kind = "framework")]
    unsafe extern "C" {
        fn CGPreflightListenEventAccess() -> bool;
        fn CGEventTapCreate(
            tap: u32,
            place: u32,
            options: u32,
            events_of_interest: u64,
            callback: unsafe extern "C" fn(
                *mut c_void,
                u32,
                *mut c_void,
                *mut c_void,
            ) -> *mut c_void,
            user_info: *mut c_void,
        ) -> *mut c_void;
        fn CGEventTapEnable(tap: *mut c_void, enable: bool);
        fn CGEventGetIntegerValueField(event: *mut c_void, field: u32) -> i64;
        fn CGEventGetFlags(event: *mut c_void) -> u64;
    }

    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFMachPortCreateRunLoopSource(
            allocator: *const c_void,
            port: *mut c_void,
            order: isize,
        ) -> *mut c_void;
        fn CFRunLoopGetCurrent() -> *mut c_void;
        fn CFRunLoopAddSource(rl: *mut c_void, source: *mut c_void, mode: *const c_void);
        fn CFRunLoopRun();
        static kCFRunLoopCommonModes: *const c_void;
    }

    const K_CG_HID_EVENT_TAP: u32 = 0;
    const K_CG_HEAD_INSERT_EVENT_TAP: u32 = 0;
    const K_CG_EVENT_TAP_OPTION_LISTEN_ONLY: u32 = 1;
    const K_CG_EVENT_KEY_DOWN: u32 = 10;
    const K_CG_EVENT_KEY_UP: u32 = 11;
    const K_CG_EVENT_FLAGS_CHANGED: u32 = 12;
    const K_CG_KEYBOARD_EVENT_KEYCODE: u32 = 9;

    const NX_DEVICERALTKEYMASK: u64 = 0x0000_0040;
    const NX_DEVICELALTKEYMASK: u64 = 0x0000_0020;
    const NX_DEVICERCTLKEYMASK: u64 = 0x0000_2000;
    const NX_DEVICELCTLKEYMASK: u64 = 0x0000_0001;
    const NX_DEVICERCMDKEYMASK: u64 = 0x0000_0010;
    const NX_DEVICELCMDKEYMASK: u64 = 0x0000_0008;
    const K_CG_EVENT_FLAG_MASK_SECONDARY_FN: u64 = 0x0080_0000;

    pub fn is_input_monitoring_trusted() -> bool {
        unsafe { CGPreflightListenEventAccess() }
    }

    enum MacKeySpec {
        Modifier { keycode: i64, flag_mask: u64 },
        Regular { keycode: i64 },
    }

    fn key_spec(key: ActivationKey) -> MacKeySpec {
        match key {
            ActivationKey::RightOption => MacKeySpec::Modifier {
                keycode: 61,
                flag_mask: NX_DEVICERALTKEYMASK,
            },
            ActivationKey::LeftOption => MacKeySpec::Modifier {
                keycode: 58,
                flag_mask: NX_DEVICELALTKEYMASK,
            },
            ActivationKey::RightControl => MacKeySpec::Modifier {
                keycode: 62,
                flag_mask: NX_DEVICERCTLKEYMASK,
            },
            ActivationKey::LeftControl => MacKeySpec::Modifier {
                keycode: 59,
                flag_mask: NX_DEVICELCTLKEYMASK,
            },
            ActivationKey::RightCommand => MacKeySpec::Modifier {
                keycode: 54,
                flag_mask: NX_DEVICERCMDKEYMASK,
            },
            ActivationKey::LeftCommand => MacKeySpec::Modifier {
                keycode: 55,
                flag_mask: NX_DEVICELCMDKEYMASK,
            },
            ActivationKey::Fn => MacKeySpec::Modifier {
                keycode: 63,
                flag_mask: K_CG_EVENT_FLAG_MASK_SECONDARY_FN,
            },
            ActivationKey::F13 => MacKeySpec::Regular { keycode: 105 },
            ActivationKey::F14 => MacKeySpec::Regular { keycode: 107 },
            ActivationKey::F15 => MacKeySpec::Regular { keycode: 113 },
            ActivationKey::F16 => MacKeySpec::Regular { keycode: 106 },
            ActivationKey::F17 => MacKeySpec::Regular { keycode: 64 },
            ActivationKey::F18 => MacKeySpec::Regular { keycode: 79 },
            ActivationKey::F19 => MacKeySpec::Regular { keycode: 80 },
            ActivationKey::F20 => MacKeySpec::Regular { keycode: 90 },
        }
    }

    struct TapContext {
        spec: MacKeySpec,
        tx: Sender<KeyEdge>,
    }

    unsafe extern "C" fn event_tap_callback(
        _proxy: *mut c_void,
        event_type: u32,
        event: *mut c_void,
        user_info: *mut c_void,
    ) -> *mut c_void {
        let ctx = unsafe { &*(user_info as *const TapContext) };
        let keycode = unsafe { CGEventGetIntegerValueField(event, K_CG_KEYBOARD_EVENT_KEYCODE) };

        match (&ctx.spec, event_type) {
            (
                MacKeySpec::Modifier {
                    keycode: kc,
                    flag_mask,
                },
                K_CG_EVENT_FLAGS_CHANGED,
            ) if keycode == *kc => {
                let flags = unsafe { CGEventGetFlags(event) };
                let edge = if (flags & flag_mask) != 0 {
                    KeyEdge::Down
                } else {
                    KeyEdge::Up
                };
                let _ = ctx.tx.send(edge);
            }
            (MacKeySpec::Regular { keycode: kc }, K_CG_EVENT_KEY_DOWN) if keycode == *kc => {
                let _ = ctx.tx.send(KeyEdge::Down);
            }
            (MacKeySpec::Regular { keycode: kc }, K_CG_EVENT_KEY_UP) if keycode == *kc => {
                let _ = ctx.tx.send(KeyEdge::Up);
            }
            _ => {}
        }
        event
    }

    pub fn spawn_hook(key: ActivationKey, tx: Sender<KeyEdge>) -> Result<(), KikuError> {
        let spec = key_spec(key);
        thread::spawn(move || unsafe {
            let ctx = Box::into_raw(Box::new(TapContext { spec, tx }));
            let mask = (1u64 << K_CG_EVENT_KEY_DOWN)
                | (1u64 << K_CG_EVENT_KEY_UP)
                | (1u64 << K_CG_EVENT_FLAGS_CHANGED);
            let tap = CGEventTapCreate(
                K_CG_HID_EVENT_TAP,
                K_CG_HEAD_INSERT_EVENT_TAP,
                K_CG_EVENT_TAP_OPTION_LISTEN_ONLY,
                mask,
                event_tap_callback,
                ctx.cast(),
            );
            if tap.is_null() {
                eprintln!("[kiku] failed to create CGEventTap (check Input Monitoring permission)");
                return;
            }
            let source = CFMachPortCreateRunLoopSource(std::ptr::null(), tap, 0);
            let rl = CFRunLoopGetCurrent();
            CFRunLoopAddSource(rl, source, kCFRunLoopCommonModes);
            CGEventTapEnable(tap, true);
            CFRunLoopRun();
        });
        Ok(())
    }
}

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
mod platform {
    use std::sync::mpsc::Sender;

    use super::{ActivationKey, KeyEdge};
    use crate::ports::KikuError;

    pub fn is_input_monitoring_trusted() -> bool {
        true
    }

    pub fn spawn_hook(_key: ActivationKey, _tx: Sender<KeyEdge>) -> Result<(), KikuError> {
        Err(KikuError::Config(
            "global Activation Key hook is only supported on macOS and Windows".to_owned(),
        ))
    }
}
