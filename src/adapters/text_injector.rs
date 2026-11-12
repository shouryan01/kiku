use std::thread;
use std::time::Duration;

use arboard::Clipboard;
use enigo::{
    Direction::{Click, Press, Release},
    Enigo, Key, Keyboard, Settings,
};

use crate::ports::{KikuError, TextInjector};

const DEFAULT_PASTE_SETTLE_DELAY: Duration = Duration::from_millis(120);

/// Abstraction over OS clipboard operations for deterministic testing.
pub trait ClipboardPort {
    fn get_text(&mut self) -> Option<String>;
    fn set_text(&mut self, text: &str) -> Result<(), KikuError>;
}

/// Abstraction over OS paste shortcut simulation for deterministic testing.
pub trait KeyPastePort {
    fn paste(&mut self) -> Result<(), KikuError>;
}

/// Checks whether the process has OS accessibility permission to simulate paste.
pub trait AccessibilityCheck {
    fn is_trusted(&self) -> bool;
}

pub struct OsClipboard {
    clipboard: Clipboard,
}

impl OsClipboard {
    pub fn new() -> Result<Self, KikuError> {
        let clipboard = Clipboard::new()
            .map_err(|e| KikuError::Injection(format!("failed to open clipboard: {e}")))?;
        Ok(Self { clipboard })
    }
}

impl ClipboardPort for OsClipboard {
    fn get_text(&mut self) -> Option<String> {
        self.clipboard.get_text().ok()
    }

    fn set_text(&mut self, text: &str) -> Result<(), KikuError> {
        self.clipboard
            .set_text(text.to_owned())
            .map_err(|e| KikuError::Injection(format!("failed to write clipboard: {e}")))
    }
}

pub struct OsKeyPaste;

impl KeyPastePort for OsKeyPaste {
    fn paste(&mut self) -> Result<(), KikuError> {
        let mut enigo = Enigo::new(&Settings::default())
            .map_err(|e| KikuError::Injection(format!("failed to init input simulator: {e}")))?;

        let modifier = if cfg!(target_os = "macos") {
            Key::Meta
        } else {
            Key::Control
        };

        enigo
            .key(modifier, Press)
            .map_err(|e| KikuError::Injection(format!("failed to press paste modifier: {e}")))?;
        let click_res = enigo.key(Key::Unicode('v'), Click);
        let release_res = enigo.key(modifier, Release);

        click_res.map_err(|e| KikuError::Injection(format!("failed to press 'v': {e}")))?;
        release_res
            .map_err(|e| KikuError::Injection(format!("failed to release paste modifier: {e}")))?;
        Ok(())
    }
}

pub struct OsAccessibilityCheck;

impl AccessibilityCheck for OsAccessibilityCheck {
    fn is_trusted(&self) -> bool {
        is_accessibility_trusted()
    }
}

#[cfg(target_os = "macos")]
pub fn is_accessibility_trusted() -> bool {
    #[link(name = "ApplicationServices", kind = "framework")]
    unsafe extern "C" {
        fn AXIsProcessTrusted() -> bool;
    }
    unsafe { AXIsProcessTrusted() }
}

#[cfg(not(target_os = "macos"))]
pub fn is_accessibility_trusted() -> bool {
    true
}

/// Injects text by snapshotting the clipboard, pasting via Cmd+V / Ctrl+V, and restoring it.
pub struct ClipboardTextInjector<C, K, A> {
    clipboard: C,
    paster: K,
    accessibility: A,
    settle_delay: Duration,
}

impl ClipboardTextInjector<OsClipboard, OsKeyPaste, OsAccessibilityCheck> {
    pub fn new_os() -> Result<Self, KikuError> {
        Ok(Self::new(
            OsClipboard::new()?,
            OsKeyPaste,
            OsAccessibilityCheck,
            DEFAULT_PASTE_SETTLE_DELAY,
        ))
    }
}

impl<C, K, A> ClipboardTextInjector<C, K, A>
where
    C: ClipboardPort,
    K: KeyPastePort,
    A: AccessibilityCheck,
{
    pub fn new(clipboard: C, paster: K, accessibility: A, settle_delay: Duration) -> Self {
        Self {
            clipboard,
            paster,
            accessibility,
            settle_delay,
        }
    }
}

impl<C, K, A> TextInjector for ClipboardTextInjector<C, K, A>
where
    C: ClipboardPort,
    K: KeyPastePort,
    A: AccessibilityCheck,
{
    fn inject(&mut self, text: &str) -> Result<(), KikuError> {
        if !self.accessibility.is_trusted() {
            return Err(KikuError::Injection(
                "macOS Accessibility permission is required for pasting. Enable kiku in System Settings -> Privacy & Security -> Accessibility.".to_owned(),
            ));
        }

        let previous = self.clipboard.get_text();
        self.clipboard.set_text(text)?;

        let paste_res = self.paster.paste();
        if !self.settle_delay.is_zero() {
            thread::sleep(self.settle_delay);
        }

        if let Some(prev_text) = previous {
            let _ = self.clipboard.set_text(&prev_text);
        }

        paste_res
    }
}
