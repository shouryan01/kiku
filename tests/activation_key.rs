use std::cell::Cell;
use std::rc::Rc;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use kiku::adapters::activation_key::{
    ActivationKey, HoldAction, HoldKeyTracker, KeyEdge, run_hold_loop,
};
use kiku::ports::{
    AudioBuffer, AudioFormat, AudioInput, CapturedAudio, KikuError, MAX_SESSION_DURATION,
    MediaController, PostProcessor, STTProvider, SessionState, StatusNotifier, TextInjector,
};
use kiku::session::DictationCoordinator;

#[test]
fn parses_default_and_aliased_activation_keys() {
    assert_eq!(
        ActivationKey::parse("RightOption").unwrap(),
        ActivationKey::RightOption
    );
    assert_eq!(
        ActivationKey::parse("right_alt").unwrap(),
        ActivationKey::RightOption
    );
    assert_eq!(ActivationKey::parse("fn").unwrap(), ActivationKey::Fn);
    assert_eq!(ActivationKey::parse("Globe").unwrap(), ActivationKey::Fn);
    assert_eq!(ActivationKey::parse("F13").unwrap(), ActivationKey::F13);
    assert!(ActivationKey::parse("NotAKey").is_err());
}

#[test]
fn hold_tracker_ignores_tap_under_250ms_without_starting_session() {
    let mut tracker = HoldKeyTracker::new();
    let t0 = Instant::now();

    tracker.on_key_down(t0);
    assert_eq!(
        tracker.on_tick(t0 + Duration::from_millis(100)),
        HoldAction::None
    );
    assert_eq!(
        tracker.on_key_up(t0 + Duration::from_millis(200)),
        HoldAction::None
    );
}

#[test]
fn hold_tracker_starts_after_250ms_ignores_repeats_and_finishes_on_release() {
    let mut tracker = HoldKeyTracker::new();
    let t0 = Instant::now();

    tracker.on_key_down(t0);
    // Duplicate key-down from OS auto-repeat does not reset press timestamp
    tracker.on_key_down(t0 + Duration::from_millis(150));

    assert_eq!(
        tracker.on_tick(t0 + Duration::from_millis(260)),
        HoldAction::StartSession
    );
    assert_eq!(
        tracker.on_tick(t0 + Duration::from_millis(400)),
        HoldAction::None
    );

    assert_eq!(
        tracker.on_key_up(t0 + Duration::from_millis(900)),
        HoldAction::FinishSession(Duration::from_millis(900))
    );
}

#[test]
fn hold_tracker_caps_at_max_session_duration() {
    let mut tracker = HoldKeyTracker::new();
    let t0 = Instant::now();

    tracker.on_key_down(t0);
    assert_eq!(
        tracker.on_tick(t0 + Duration::from_millis(300)),
        HoldAction::StartSession
    );
    assert_eq!(
        tracker.on_tick(t0 + MAX_SESSION_DURATION),
        HoldAction::FinishSession(MAX_SESSION_DURATION)
    );
    assert_eq!(
        tracker.on_key_up(t0 + MAX_SESSION_DURATION + Duration::from_secs(5)),
        HoldAction::None
    );
}

struct CountingAudio {
    starts: Rc<Cell<usize>>,
}
impl AudioInput for CountingAudio {
    fn start_capture(&mut self) -> Result<(), KikuError> {
        self.starts.set(self.starts.get() + 1);
        Ok(())
    }
    fn stop_capture(&mut self) -> Result<CapturedAudio, KikuError> {
        Ok(CapturedAudio::Encoded(AudioBuffer {
            bytes: vec![1, 2, 3],
            format: AudioFormat::Wav,
        }))
    }
}

struct CountingStt {
    calls: Rc<Cell<usize>>,
}
impl STTProvider for CountingStt {
    fn transcribe(&self, _audio: &AudioBuffer) -> Result<String, KikuError> {
        self.calls.set(self.calls.get() + 1);
        Ok("ok".to_owned())
    }
}

struct NoopPost;
impl PostProcessor for NoopPost {
    fn clean(&self, raw: &str) -> Result<String, KikuError> {
        Ok(raw.to_owned())
    }
}

struct NoopInjector;
impl TextInjector for NoopInjector {
    fn inject(&mut self, _text: &str) -> Result<(), KikuError> {
        Ok(())
    }
}

struct CountingMedia {
    pauses: Rc<Cell<usize>>,
}
impl MediaController for CountingMedia {
    fn pause_if_playing(&mut self) -> bool {
        self.pauses.set(self.pauses.get() + 1);
        true
    }
    fn resume(&mut self) {}
}

struct NoopNotifier;
impl StatusNotifier for NoopNotifier {
    fn notify(&mut self, _state: SessionState) {}
}

#[test]
fn run_hold_loop_ignores_short_tap_without_starting_audio_or_pausing_media() {
    let audio_starts = Rc::new(Cell::new(0));
    let stt_calls = Rc::new(Cell::new(0));
    let media_pauses = Rc::new(Cell::new(0));

    let mut coordinator = DictationCoordinator::new(
        CountingAudio {
            starts: Rc::clone(&audio_starts),
        },
        CountingStt {
            calls: Rc::clone(&stt_calls),
        },
        NoopPost,
        NoopInjector,
        CountingMedia {
            pauses: Rc::clone(&media_pauses),
        },
        NoopNotifier,
    );

    let (tx, rx) = mpsc::channel();
    tx.send(KeyEdge::Down).unwrap();
    tx.send(KeyEdge::Up).unwrap();
    drop(tx);

    run_hold_loop(&mut coordinator, &rx);

    assert_eq!(audio_starts.get(), 0);
    assert_eq!(media_pauses.get(), 0);
    assert_eq!(stt_calls.get(), 0);
}
