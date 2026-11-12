use std::path::Path;
use std::process::Command;
use std::sync::mpsc::{Receiver, Sender, TryRecvError};
use std::thread;
use std::time::Duration;

use muda::{Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder};

use crate::adapters::activation_key::WorkerControl;
use crate::config::KikuConfig;
use crate::ports::{KikuError, SessionState, StatusNotifier};

pub const TRAY_ICON_SIZE: u32 = 16;

/// Commands triggered from the native tray/menu bar menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayCommand {
    RetryLastDictation,
    OpenConfig,
    Quit,
}

/// Pure, platform-independent tray state model updated from SessionState transitions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TrayStateModel {
    pub state: SessionState,
    pub retry_enabled: bool,
}

impl Default for TrayStateModel {
    fn default() -> Self {
        Self {
            state: SessionState::Idle,
            retry_enabled: false,
        }
    }
}

impl TrayStateModel {
    pub fn apply(&mut self, state: SessionState) {
        self.state = state;
        self.retry_enabled = matches!(state, SessionState::RetryAvailable);
    }

    pub fn status_label(&self) -> &'static str {
        match self.state {
            SessionState::Idle => "Status: Idle",
            SessionState::Recording => "Status: Recording...",
            SessionState::Processing => "Status: Processing...",
            SessionState::Error => "Status: Error",
            SessionState::RetryAvailable => "Status: Retry Available",
        }
    }

    pub fn tooltip(&self) -> &'static str {
        match self.state {
            SessionState::Idle => "kiku — Idle",
            SessionState::Recording => "kiku — Recording",
            SessionState::Processing => "kiku — Processing",
            SessionState::Error => "kiku — Error",
            SessionState::RetryAvailable => "kiku — Retry Available",
        }
    }
}

/// StatusNotifier implementation that forwards SessionState updates to the tray UI thread.
pub struct TrayStatusNotifier {
    tx: Sender<SessionState>,
}

impl TrayStatusNotifier {
    pub fn new(tx: Sender<SessionState>) -> Self {
        Self { tx }
    }
}

impl StatusNotifier for TrayStatusNotifier {
    fn notify(&mut self, state: SessionState) {
        let _ = self.tx.send(state);
    }
}

/// Generates a lightweight 16x16 RGBA icon (1,024 bytes) reflecting the current SessionState.
pub fn render_state_icon_rgba(state: SessionState) -> Vec<u8> {
    let size = TRAY_ICON_SIZE as i32;
    let mut rgba = vec![0_u8; (TRAY_ICON_SIZE * TRAY_ICON_SIZE * 4) as usize];

    let color: [u8; 4] = match state {
        SessionState::Idle => [225, 229, 235, 255],
        SessionState::Recording => [239, 68, 68, 255],
        SessionState::Processing => [245, 158, 11, 255],
        SessionState::Error => [220, 38, 38, 255],
        SessionState::RetryAvailable => [56, 189, 248, 255],
    };

    for y in 0..size {
        for x in 0..size {
            let dx = (x * 2 + 1) - size;
            let dy = (y * 2 + 1) - size;
            let dist_sq = dx * dx + dy * dy;

            let paint = match state {
                // Hollow ring for Idle and RetryAvailable, solid dot for Recording/Processing,
                // and an X mark for Error so states remain distinguishable even without color.
                SessionState::Idle | SessionState::RetryAvailable => (64..=169).contains(&dist_sq),
                SessionState::Recording | SessionState::Processing => dist_sq <= 144,
                SessionState::Error => {
                    let on_diag = (x - y).abs() <= 1 || ((size - 1 - x) - y).abs() <= 1;
                    let in_bounds = (3..size - 3).contains(&x) && (3..size - 3).contains(&y);
                    on_diag && in_bounds
                }
            };

            if paint {
                let idx = ((y * size + x) * 4) as usize;
                rgba[idx..idx + 4].copy_from_slice(&color);
            }
        }
    }

    rgba
}

fn build_tray_icon(state: SessionState) -> Result<Icon, KikuError> {
    let rgba = render_state_icon_rgba(state);
    Icon::from_rgba(rgba, TRAY_ICON_SIZE, TRAY_ICON_SIZE)
        .map_err(|e| KikuError::Config(format!("failed to build tray icon: {e}")))
}

/// Ensures the TOML config exists and opens it in the OS default editor.
pub fn open_config_in_editor(path: &Path) -> Result<(), KikuError> {
    KikuConfig::write_default_if_missing(path)?;

    #[cfg(target_os = "macos")]
    let mut cmd = {
        let mut c = Command::new("open");
        c.arg(path);
        c
    };

    #[cfg(target_os = "windows")]
    let mut cmd = {
        let mut c = Command::new("cmd");
        c.args(["/C", "start", ""]).arg(path);
        c
    };

    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let mut cmd = {
        let mut c = Command::new("xdg-open");
        c.arg(path);
        c
    };

    cmd.spawn()
        .map(|_| ())
        .map_err(|e| KikuError::Config(format!("failed to open config {}: {e}", path.display())))
}

/// Native menu bar (macOS) / system tray (Windows) wrapper around `tray-icon` + `muda`.
pub struct NativeTray {
    tray_icon: TrayIcon,
    status_item: MenuItem,
    retry_item: MenuItem,
    open_config_item: MenuItem,
    quit_item: MenuItem,
    _menu: Menu,
    model: TrayStateModel,
}

impl NativeTray {
    pub fn new() -> Result<Self, KikuError> {
        platform::init_ui_thread();

        let model = TrayStateModel::default();
        let status_item = MenuItem::new(model.status_label(), false, None);
        let retry_item = MenuItem::new("Retry Last Dictation", model.retry_enabled, None);
        let open_config_item = MenuItem::new("Open Config", true, None);
        let quit_item = MenuItem::new("Quit", true, None);

        let menu = Menu::new();
        menu.append_items(&[
            &status_item,
            &PredefinedMenuItem::separator(),
            &retry_item,
            &open_config_item,
            &PredefinedMenuItem::separator(),
            &quit_item,
        ])
        .map_err(|e| KikuError::Config(format!("failed to build tray menu: {e}")))?;

        let icon = build_tray_icon(model.state)?;
        let tray_icon = TrayIconBuilder::new()
            .with_menu(Box::new(menu.clone()))
            .with_tooltip(model.tooltip())
            .with_icon(icon)
            .build()
            .map_err(|e| KikuError::Config(format!("failed to create tray icon: {e}")))?;

        Ok(Self {
            tray_icon,
            status_item,
            retry_item,
            open_config_item,
            quit_item,
            _menu: menu,
            model,
        })
    }

    pub fn apply_state(&mut self, state: SessionState) -> Result<(), KikuError> {
        self.model.apply(state);
        let icon = build_tray_icon(state)?;
        self.tray_icon
            .set_icon(Some(icon))
            .map_err(|e| KikuError::Config(format!("failed to update tray icon: {e}")))?;
        let _ = self.tray_icon.set_tooltip(Some(self.model.tooltip()));
        self.status_item.set_text(self.model.status_label());
        self.retry_item.set_enabled(self.model.retry_enabled);
        Ok(())
    }

    pub fn map_menu_event(&self, id: &MenuId) -> Option<TrayCommand> {
        if id == self.retry_item.id() {
            Some(TrayCommand::RetryLastDictation)
        } else if id == self.open_config_item.id() {
            Some(TrayCommand::OpenConfig)
        } else if id == self.quit_item.id() {
            Some(TrayCommand::Quit)
        } else {
            None
        }
    }

    pub fn model(&self) -> TrayStateModel {
        self.model
    }
}

/// Dispatches a TrayCommand using the worker control channel and config path.
/// Returns `true` when the tray loop should exit.
pub fn handle_tray_command(
    command: TrayCommand,
    control_tx: &Sender<WorkerControl>,
    config_path: &Path,
) -> bool {
    match command {
        TrayCommand::RetryLastDictation => {
            let _ = control_tx.send(WorkerControl::Retry);
            false
        }
        TrayCommand::OpenConfig => {
            if let Err(err) = open_config_in_editor(config_path) {
                eprintln!("[kiku] {err}");
            }
            false
        }
        TrayCommand::Quit => {
            let _ = control_tx.send(WorkerControl::Quit);
            true
        }
    }
}

/// Runs the native tray event loop on the main thread until Quit is selected or the worker exits.
pub fn run_tray_event_loop(
    state_rx: &Receiver<SessionState>,
    control_tx: &Sender<WorkerControl>,
    config_path: &Path,
) -> Result<(), KikuError> {
    let mut tray = NativeTray::new()?;
    let menu_rx = MenuEvent::receiver();

    loop {
        platform::pump_os_events();

        loop {
            match state_rx.try_recv() {
                Ok(state) => {
                    if let Err(err) = tray.apply_state(state) {
                        eprintln!("[kiku] tray state update error: {err}");
                    }
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => return Ok(()),
            }
        }

        while let Ok(event) = menu_rx.try_recv() {
            if let Some(cmd) = tray.map_menu_event(&event.id)
                && handle_tray_command(cmd, control_tx, config_path)
            {
                return Ok(());
            }
        }

        thread::sleep(Duration::from_millis(16));
    }
}

#[cfg(target_os = "windows")]
mod platform {
    use windows::Win32::UI::WindowsAndMessaging::{
        DispatchMessageW, MSG, PM_REMOVE, PeekMessageW, TranslateMessage,
    };

    pub fn init_ui_thread() {}

    pub fn pump_os_events() {
        unsafe {
            let mut msg = MSG::default();
            while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use std::ffi::c_void;

    #[link(name = "AppKit", kind = "framework")]
    unsafe extern "C" {
        fn NSApplicationLoad() -> bool;
    }

    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        static kCFRunLoopDefaultMode: *const c_void;
        fn CFRunLoopRunInMode(
            mode: *const c_void,
            seconds: f64,
            return_after_source_handled: u8,
        ) -> i32;
    }

    pub fn init_ui_thread() {
        unsafe {
            let _ = NSApplicationLoad();
        }
    }

    pub fn pump_os_events() {
        unsafe {
            let _ = CFRunLoopRunInMode(kCFRunLoopDefaultMode, 0.005, 1);
        }
    }
}

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
mod platform {
    pub fn init_ui_thread() {}
    pub fn pump_os_events() {}
}
