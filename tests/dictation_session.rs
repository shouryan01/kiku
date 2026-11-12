use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use kiku::config::KikuConfig;
use kiku::ports::{
    AudioBuffer, AudioFormat, AudioInput, CapturedAudio, KikuError, MAX_AUDIO_BYTES,
    MAX_SESSION_DURATION, MediaController, PostProcessor, RawAudio, STTProvider, SessionOutcome,
    SessionState, StatusNotifier, TextInjector,
};
use kiku::session::DictationCoordinator;

struct FakeAudio {
    captured: CapturedAudio,
    fail_start: bool,
    fail_stop: bool,
}

impl FakeAudio {
    fn new(bytes: Vec<u8>) -> Self {
        Self {
            captured: CapturedAudio::Encoded(AudioBuffer {
                bytes,
                format: AudioFormat::Wav,
            }),
            fail_start: false,
            fail_stop: false,
        }
    }

    fn raw(raw: RawAudio) -> Self {
        Self {
            captured: CapturedAudio::Raw(raw),
            fail_start: false,
            fail_stop: false,
        }
    }
}

impl AudioInput for FakeAudio {
    fn start_capture(&mut self) -> Result<(), KikuError> {
        if self.fail_start {
            return Err(KikuError::AudioCapture("mic unavailable".to_owned()));
        }
        Ok(())
    }
    fn stop_capture(&mut self) -> Result<CapturedAudio, KikuError> {
        if self.fail_stop {
            return Err(KikuError::AudioCapture("mic disconnected".to_owned()));
        }
        Ok(self.captured.clone())
    }
}

struct FakeStt {
    calls: Rc<Cell<usize>>,
    last_len: Rc<Cell<usize>>,
    results: RefCell<Vec<Result<String, KikuError>>>,
}

impl FakeStt {
    fn ok(text: &str) -> (Self, Rc<Cell<usize>>, Rc<Cell<usize>>) {
        let calls = Rc::new(Cell::new(0));
        let last_len = Rc::new(Cell::new(0));
        (
            Self {
                calls: Rc::clone(&calls),
                last_len: Rc::clone(&last_len),
                results: RefCell::new(vec![Ok(text.to_owned())]),
            },
            calls,
            last_len,
        )
    }

    fn sequence(results: Vec<Result<String, KikuError>>) -> (Self, Rc<Cell<usize>>) {
        let calls = Rc::new(Cell::new(0));
        (
            Self {
                calls: Rc::clone(&calls),
                last_len: Rc::new(Cell::new(0)),
                results: RefCell::new(results),
            },
            calls,
        )
    }
}

impl STTProvider for FakeStt {
    fn transcribe(&self, audio: &AudioBuffer) -> Result<String, KikuError> {
        self.calls.set(self.calls.get() + 1);
        self.last_len.set(audio.bytes.len());
        let mut queue = self.results.borrow_mut();
        if queue.len() > 1 {
            queue.remove(0)
        } else {
            queue[0].clone()
        }
    }
}

struct FakePost {
    result: Result<String, KikuError>,
}

impl FakePost {
    fn ok(text: &str) -> Self {
        Self {
            result: Ok(text.to_owned()),
        }
    }

    fn err(msg: &str) -> Self {
        Self {
            result: Err(KikuError::PostProcess(msg.to_owned())),
        }
    }
}

impl PostProcessor for FakePost {
    fn clean(&self, _raw_transcript: &str) -> Result<String, KikuError> {
        self.result.clone()
    }
}

#[derive(Default)]
struct FakeInjector {
    injected: Rc<RefCell<Vec<String>>>,
    fail: bool,
}

impl TextInjector for FakeInjector {
    fn inject(&mut self, text: &str) -> Result<(), KikuError> {
        if self.fail {
            return Err(KikuError::Injection("clipboard locked".to_owned()));
        }
        self.injected.borrow_mut().push(text.to_owned());
        Ok(())
    }
}

#[derive(Default)]
struct FakeMedia {
    is_playing: bool,
    paused_calls: Rc<Cell<usize>>,
    resumed_calls: Rc<Cell<usize>>,
}

impl FakeMedia {
    fn playing() -> (Self, Rc<Cell<usize>>, Rc<Cell<usize>>) {
        let paused_calls = Rc::new(Cell::new(0));
        let resumed_calls = Rc::new(Cell::new(0));
        (
            Self {
                is_playing: true,
                paused_calls: Rc::clone(&paused_calls),
                resumed_calls: Rc::clone(&resumed_calls),
            },
            paused_calls,
            resumed_calls,
        )
    }
}

impl MediaController for FakeMedia {
    fn pause_if_playing(&mut self) -> bool {
        if self.is_playing {
            self.is_playing = false;
            self.paused_calls.set(self.paused_calls.get() + 1);
            true
        } else {
            false
        }
    }
    fn resume(&mut self) {
        self.is_playing = true;
        self.resumed_calls.set(self.resumed_calls.get() + 1);
    }
}

#[derive(Default)]
struct FakeNotifier {
    states: Rc<RefCell<Vec<SessionState>>>,
}

impl StatusNotifier for FakeNotifier {
    fn notify(&mut self, state: SessionState) {
        self.states.borrow_mut().push(state);
    }
}

#[test]
fn dictation_session_happy_path_transcribes_cleans_and_injects() {
    let (stt, stt_calls, _) = FakeStt::ok("um hello world");
    let (media, paused, resumed) = FakeMedia::playing();
    let injector = FakeInjector::default();
    let injected = Rc::clone(&injector.injected);
    let notifier = FakeNotifier::default();
    let states = Rc::clone(&notifier.states);

    let mut coordinator = DictationCoordinator::new(
        FakeAudio::new(vec![1, 2, 3, 4]),
        stt,
        FakePost::ok("Hello world."),
        injector,
        media,
        notifier,
    );

    coordinator.start_session().unwrap();
    let outcome = coordinator
        .finish_session(Duration::from_millis(600))
        .unwrap();

    assert_eq!(
        outcome,
        SessionOutcome::Injected {
            text: "Hello world.".to_owned(),
            used_raw_fallback: false,
        }
    );
    assert_eq!(stt_calls.get(), 1);
    assert_eq!(injected.borrow().as_slice(), &["Hello world."]);
    assert_eq!(paused.get(), 1);
    assert_eq!(resumed.get(), 1);
    assert_eq!(
        states.borrow().as_slice(),
        &[
            SessionState::Recording,
            SessionState::Processing,
            SessionState::Idle
        ]
    );
}

#[test]
fn media_is_not_resumed_when_not_playing_at_session_start() {
    let (stt, _, _) = FakeStt::ok("hello");
    let media = FakeMedia::default();
    let paused = Rc::clone(&media.paused_calls);
    let resumed = Rc::clone(&media.resumed_calls);

    let mut coordinator = DictationCoordinator::new(
        FakeAudio::new(vec![1, 2, 3]),
        stt,
        FakePost::ok("Hello."),
        FakeInjector::default(),
        media,
        FakeNotifier::default(),
    );

    coordinator.start_session().unwrap();
    coordinator
        .finish_session(Duration::from_millis(400))
        .unwrap();

    assert_eq!(paused.get(), 0);
    assert_eq!(resumed.get(), 0);
}

#[test]
fn short_tap_under_250ms_is_ignored_without_calling_stt() {
    let (stt, stt_calls, _) = FakeStt::ok("should not run");
    let (media, paused, resumed) = FakeMedia::playing();
    let notifier = FakeNotifier::default();
    let states = Rc::clone(&notifier.states);

    let mut coordinator = DictationCoordinator::new(
        FakeAudio::new(vec![1, 2, 3]),
        stt,
        FakePost::ok("ignored"),
        FakeInjector::default(),
        media,
        notifier,
    );

    coordinator.start_session().unwrap();
    let outcome = coordinator
        .finish_session(Duration::from_millis(200))
        .unwrap();

    assert_eq!(outcome, SessionOutcome::IgnoredTap);
    assert_eq!(stt_calls.get(), 0);
    assert_eq!(paused.get(), 1);
    assert_eq!(resumed.get(), 1);
    assert_eq!(
        states.borrow().as_slice(),
        &[SessionState::Recording, SessionState::Idle]
    );
}

#[test]
fn empty_audio_aborts_locally_as_no_speech_and_resumes_media() {
    let (stt, stt_calls, _) = FakeStt::ok("should not run");
    let (media, _, resumed) = FakeMedia::playing();

    let mut coordinator = DictationCoordinator::new(
        FakeAudio::new(vec![]),
        stt,
        FakePost::ok("should not run"),
        FakeInjector::default(),
        media,
        FakeNotifier::default(),
    );

    coordinator.start_session().unwrap();
    let outcome = coordinator
        .finish_session(Duration::from_millis(500))
        .unwrap();

    assert_eq!(outcome, SessionOutcome::NoSpeech);
    assert_eq!(stt_calls.get(), 0);
    assert_eq!(resumed.get(), 1);
}

#[test]
fn raw_silence_via_audio_pipeline_aborts_locally_and_resumes_media() {
    let (stt, stt_calls, _) = FakeStt::ok("should not run");
    let (media, _, resumed) = FakeMedia::playing();
    let silence = RawAudio {
        samples: vec![0.0; 24_000],
        sample_rate: 48_000,
        channels: 1,
    };

    let mut coordinator = DictationCoordinator::new(
        FakeAudio::raw(silence),
        stt,
        FakePost::ok("should not run"),
        FakeInjector::default(),
        media,
        FakeNotifier::default(),
    );

    coordinator.start_session().unwrap();
    let outcome = coordinator
        .finish_session(Duration::from_millis(500))
        .unwrap();

    assert_eq!(outcome, SessionOutcome::NoSpeech);
    assert_eq!(stt_calls.get(), 0);
    assert_eq!(resumed.get(), 1);
}

#[test]
fn oversized_audio_is_capped_to_max_audio_bytes() {
    let oversized = vec![7u8; MAX_AUDIO_BYTES + 4096];
    let (stt, _, last_len) = FakeStt::ok("hello");

    let mut coordinator = DictationCoordinator::new(
        FakeAudio::new(oversized),
        stt,
        FakePost::ok("Hello."),
        FakeInjector::default(),
        FakeMedia::default(),
        FakeNotifier::default(),
    );

    coordinator.start_session().unwrap();
    coordinator.finish_session(Duration::from_secs(10)).unwrap();

    assert_eq!(last_len.get(), MAX_AUDIO_BYTES);
    assert!(DictationCoordinator::<
        FakeAudio,
        FakeStt,
        FakePost,
        FakeInjector,
        FakeMedia,
        FakeNotifier,
    >::is_duration_capped(MAX_SESSION_DURATION));
    assert!(!DictationCoordinator::<
        FakeAudio,
        FakeStt,
        FakePost,
        FakeInjector,
        FakeMedia,
        FakeNotifier,
    >::is_duration_capped(Duration::from_secs(60)));
}

#[test]
fn post_processor_failure_falls_back_to_raw_stt_transcript() {
    let (stt, _, _) = FakeStt::ok("um raw transcript");
    let mut coordinator = DictationCoordinator::new(
        FakeAudio::new(vec![1, 2, 3]),
        stt,
        FakePost::err("rate limit"),
        FakeInjector::default(),
        FakeMedia::default(),
        FakeNotifier::default(),
    );

    coordinator.start_session().unwrap();
    let outcome = coordinator
        .finish_session(Duration::from_millis(700))
        .unwrap();

    assert_eq!(
        outcome,
        SessionOutcome::Injected {
            text: "um raw transcript".to_owned(),
            used_raw_fallback: true,
        }
    );
}

#[test]
fn stt_failure_retains_audio_for_retry_and_new_session_clears_it() {
    let (stt, stt_calls) = FakeStt::sequence(vec![
        Err(KikuError::Stt("network timeout".to_owned())),
        Ok("um retry worked".to_owned()),
    ]);
    let (media, _, resumed) = FakeMedia::playing();
    let notifier = FakeNotifier::default();
    let states = Rc::clone(&notifier.states);

    let mut coordinator = DictationCoordinator::new(
        FakeAudio::new(vec![9, 8, 7]),
        stt,
        FakePost::ok("Retry worked."),
        FakeInjector::default(),
        media,
        notifier,
    );

    coordinator.start_session().unwrap();
    let err = coordinator
        .finish_session(Duration::from_millis(800))
        .unwrap_err();
    assert_eq!(err, KikuError::Stt("network timeout".to_owned()));
    assert!(coordinator.has_retry_buffer());
    assert_eq!(resumed.get(), 1);
    assert_eq!(
        states.borrow().last().copied(),
        Some(SessionState::RetryAvailable)
    );

    let retry_outcome = coordinator.retry_last_session().unwrap();
    assert_eq!(
        retry_outcome,
        SessionOutcome::Injected {
            text: "Retry worked.".to_owned(),
            used_raw_fallback: false,
        }
    );
    assert!(!coordinator.has_retry_buffer());
    assert_eq!(stt_calls.get(), 2);
}

#[test]
fn new_session_overwrites_previous_failed_retry_buffer() {
    let (stt, _) = FakeStt::sequence(vec![
        Err(KikuError::Stt("first failed".to_owned())),
        Ok("second ok".to_owned()),
    ]);
    let mut coordinator = DictationCoordinator::new(
        FakeAudio::new(vec![1, 2, 3]),
        stt,
        FakePost::ok("Second ok."),
        FakeInjector::default(),
        FakeMedia::default(),
        FakeNotifier::default(),
    );

    coordinator.start_session().unwrap();
    assert!(
        coordinator
            .finish_session(Duration::from_millis(500))
            .is_err()
    );
    assert!(coordinator.has_retry_buffer());

    coordinator.start_session().unwrap();
    coordinator
        .finish_session(Duration::from_millis(500))
        .unwrap();
    assert!(!coordinator.has_retry_buffer());
}

#[test]
fn session_lifecycle_errors_and_adapter_failures_are_handled_cleanly() {
    let (stt, _, _) = FakeStt::ok("ok");
    let mut coordinator = DictationCoordinator::new(
        FakeAudio::new(vec![1]),
        stt,
        FakePost::ok("Ok."),
        FakeInjector::default(),
        FakeMedia::default(),
        FakeNotifier::default(),
    );
    assert_eq!(
        coordinator.finish_session(Duration::from_millis(500)),
        Err(KikuError::NoActiveSession)
    );
    assert_eq!(
        coordinator.retry_last_session(),
        Err(KikuError::NothingToRetry)
    );
    coordinator.start_session().unwrap();
    assert_eq!(
        coordinator.start_session(),
        Err(KikuError::SessionAlreadyActive)
    );

    // start_capture failure resumes media & emits Error
    let (stt, _, _) = FakeStt::ok("ok");
    let (media, _, resumed) = FakeMedia::playing();
    let notifier = FakeNotifier::default();
    let states = Rc::clone(&notifier.states);
    let mut audio = FakeAudio::new(vec![1]);
    audio.fail_start = true;
    let mut c = DictationCoordinator::new(
        audio,
        stt,
        FakePost::ok("Ok."),
        FakeInjector::default(),
        media,
        notifier,
    );
    assert!(matches!(c.start_session(), Err(KikuError::AudioCapture(_))));
    assert_eq!(resumed.get(), 1);
    assert_eq!(states.borrow().as_slice(), &[SessionState::Error]);

    // stop_capture failure resumes media & emits Error
    let (stt, _, _) = FakeStt::ok("ok");
    let (media, _, resumed) = FakeMedia::playing();
    let notifier = FakeNotifier::default();
    let states = Rc::clone(&notifier.states);
    let mut audio = FakeAudio::new(vec![1]);
    audio.fail_stop = true;
    let mut c = DictationCoordinator::new(
        audio,
        stt,
        FakePost::ok("Ok."),
        FakeInjector::default(),
        media,
        notifier,
    );
    c.start_session().unwrap();
    assert!(matches!(
        c.finish_session(Duration::from_millis(500)),
        Err(KikuError::AudioCapture(_))
    ));
    assert_eq!(resumed.get(), 1);
    assert_eq!(states.borrow().last().copied(), Some(SessionState::Error));

    // inject failure resumes media & emits Error
    let (stt, _, _) = FakeStt::ok("ok");
    let (media, _, resumed) = FakeMedia::playing();
    let notifier = FakeNotifier::default();
    let states = Rc::clone(&notifier.states);
    let injector = FakeInjector {
        fail: true,
        ..Default::default()
    };
    let mut c = DictationCoordinator::new(
        FakeAudio::new(vec![1]),
        stt,
        FakePost::ok("Ok."),
        injector,
        media,
        notifier,
    );
    c.start_session().unwrap();
    assert!(matches!(
        c.finish_session(Duration::from_millis(500)),
        Err(KikuError::Injection(_))
    ));
    assert_eq!(resumed.get(), 1);
    assert_eq!(states.borrow().last().copied(), Some(SessionState::Error));
}

#[test]
fn config_resolves_groq_api_key_from_env_before_toml_field() {
    let cfg = KikuConfig {
        groq_api_key: Some("from-toml".to_owned()),
        ..KikuConfig::default()
    };

    let from_env = cfg
        .resolve_groq_api_key_with(|k| (k == "GROQ_API_KEY").then(|| "from-env".to_owned()))
        .unwrap();
    assert_eq!(from_env, "from-env");

    let from_toml = cfg.resolve_groq_api_key_with(|_| None).unwrap();
    assert_eq!(from_toml, "from-toml");

    let missing = KikuConfig::default().resolve_groq_api_key_with(|_| None);
    assert!(matches!(missing, Err(KikuError::Config(_))));
}
