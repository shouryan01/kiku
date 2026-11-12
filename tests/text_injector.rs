use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use kiku::adapters::audio_cpal::CaptureBuffer;
use kiku::adapters::text_injector::{
    AccessibilityCheck, ClipboardPort, ClipboardTextInjector, KeyPastePort,
};
use kiku::ports::{KikuError, MAX_AUDIO_BYTES, TextInjector};

type ClipboardState = Rc<RefCell<Option<String>>>;
type ClipboardHistory = Rc<RefCell<Vec<String>>>;

struct FakeClipboard {
    current: ClipboardState,
    history: ClipboardHistory,
}

impl FakeClipboard {
    fn with_text(text: &str) -> (Self, ClipboardState, ClipboardHistory) {
        let current = Rc::new(RefCell::new(Some(text.to_owned())));
        let history = Rc::new(RefCell::new(Vec::new()));
        (
            Self {
                current: Rc::clone(&current),
                history: Rc::clone(&history),
            },
            current,
            history,
        )
    }

    fn empty() -> (Self, ClipboardState, ClipboardHistory) {
        let current = Rc::new(RefCell::new(None));
        let history = Rc::new(RefCell::new(Vec::new()));
        (
            Self {
                current: Rc::clone(&current),
                history: Rc::clone(&history),
            },
            current,
            history,
        )
    }
}

impl ClipboardPort for FakeClipboard {
    fn get_text(&mut self) -> Option<String> {
        self.current.borrow().clone()
    }

    fn set_text(&mut self, text: &str) -> Result<(), KikuError> {
        *self.current.borrow_mut() = Some(text.to_owned());
        self.history.borrow_mut().push(text.to_owned());
        Ok(())
    }
}

struct FakePaster {
    paste_calls: Rc<Cell<usize>>,
    fail: bool,
}

impl FakePaster {
    fn ok() -> (Self, Rc<Cell<usize>>) {
        let calls = Rc::new(Cell::new(0));
        (
            Self {
                paste_calls: Rc::clone(&calls),
                fail: false,
            },
            calls,
        )
    }

    fn failing() -> (Self, Rc<Cell<usize>>) {
        let calls = Rc::new(Cell::new(0));
        (
            Self {
                paste_calls: Rc::clone(&calls),
                fail: true,
            },
            calls,
        )
    }
}

impl KeyPastePort for FakePaster {
    fn paste(&mut self) -> Result<(), KikuError> {
        self.paste_calls.set(self.paste_calls.get() + 1);
        if self.fail {
            Err(KikuError::Injection("paste simulated failure".to_owned()))
        } else {
            Ok(())
        }
    }
}

struct FakeAccessibility {
    trusted: bool,
}

impl AccessibilityCheck for FakeAccessibility {
    fn is_trusted(&self) -> bool {
        self.trusted
    }
}

#[test]
fn injects_text_via_clipboard_paste_and_restores_previous_clipboard() {
    let (clipboard, current, history) = FakeClipboard::with_text("prior clipboard");
    let (paster, paste_calls) = FakePaster::ok();
    let mut injector = ClipboardTextInjector::new(
        clipboard,
        paster,
        FakeAccessibility { trusted: true },
        Duration::ZERO,
    );

    injector.inject("Dictated text.").unwrap();

    assert_eq!(paste_calls.get(), 1);
    assert_eq!(
        history.borrow().as_slice(),
        &["Dictated text.", "prior clipboard"]
    );
    assert_eq!(current.borrow().as_deref(), Some("prior clipboard"));
}

#[test]
fn injects_text_when_prior_clipboard_is_empty() {
    let (clipboard, current, history) = FakeClipboard::empty();
    let (paster, paste_calls) = FakePaster::ok();
    let mut injector = ClipboardTextInjector::new(
        clipboard,
        paster,
        FakeAccessibility { trusted: true },
        Duration::ZERO,
    );

    injector.inject("Hello!").unwrap();

    assert_eq!(paste_calls.get(), 1);
    assert_eq!(history.borrow().as_slice(), &["Hello!"]);
    assert_eq!(current.borrow().as_deref(), Some("Hello!"));
}

#[test]
fn restores_previous_clipboard_even_when_paste_fails() {
    let (clipboard, current, _) = FakeClipboard::with_text("keep me");
    let (paster, _) = FakePaster::failing();
    let mut injector = ClipboardTextInjector::new(
        clipboard,
        paster,
        FakeAccessibility { trusted: true },
        Duration::ZERO,
    );

    let err = injector.inject("new text").unwrap_err();
    assert!(matches!(err, KikuError::Injection(_)));
    assert_eq!(current.borrow().as_deref(), Some("keep me"));
}

#[test]
fn untrusted_accessibility_fails_before_touching_clipboard_or_pasting() {
    let (clipboard, current, history) = FakeClipboard::with_text("untouched");
    let (paster, paste_calls) = FakePaster::ok();
    let mut injector = ClipboardTextInjector::new(
        clipboard,
        paster,
        FakeAccessibility { trusted: false },
        Duration::ZERO,
    );

    let err = injector.inject("blocked").unwrap_err();
    assert!(err.to_string().contains("Accessibility"));
    assert_eq!(paste_calls.get(), 0);
    assert!(history.borrow().is_empty());
    assert_eq!(current.borrow().as_deref(), Some("untouched"));
}

#[test]
fn capture_buffer_enforces_10mb_sample_cap() {
    let mut buf = CaptureBuffer::new(48_000, 1);
    let max_f32_samples = MAX_AUDIO_BYTES / std::mem::size_of::<f32>();
    let chunk = vec![0.1f32; max_f32_samples + 5_000];
    buf.push_samples(&chunk);

    assert!(buf.is_full());
    assert_eq!(buf.samples.len(), max_f32_samples);
}
