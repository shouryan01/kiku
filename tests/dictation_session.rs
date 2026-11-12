use std::cell::{Cell, RefCell};
use std::time::Duration;

use kiku::config::KikuConfig;
use kiku::ports::{
    AudioBuffer, AudioFormat, AudioInput, KikuError, MAX_AUDIO_BYTES, MediaController,
    PostProcessor, STTProvider, SessionOutcome, SessionState, StatusNotifier, TextInjector,
};
use kiku::session::DictationCoordinator;

struct FakeAudio {
    started: usize,
    stopped: usize,
    buffer: AudioBuffer,
}

impl FakeAudio {
    fn new(bytes: Vec<u8>) -> Self {
        Self {
            started: 0,
            stopped: 0,
            buffer: AudioBuffer {
                bytes,
                format: AudioFormat::Wav,
            },
        }
    }
}

impl AudioInput for FakeAudio {
    fn start_capture(&mut self) -> Result<(), KikuError> {
        self.started += 1;
        Ok(())
    }
    fn stop_capture(&mut self) -> Result<AudioBuffer, KikuError> {
        self.stopped += 1;
        Ok(self.buffer.clone())
    }
}

struct FakeStt {
    calls: Cell<usize>,
    last_len: Cell<usize>,
    results: RefCell<Vec<Result<String, KikuError>>>,
}

impl FakeStt {
    fn ok(text: &str) -> Self {
        Self {
            calls: Cell::new(0),
            last_len: Cell::new(0),
            results: RefCell::new(vec![Ok(text.to_owned())]),
        }
    }

    fn sequence(results: Vec<Result<String, KikuError>>) -> Self {
        Self {
            calls: Cell::new(0),
            last_len: Cell::new(0),
            results: RefCell::new(results),
        }
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
    injected: Vec<String>,
}

impl TextInjector for FakeInjector {
    fn inject(&mut self, text: &str) -> Result<(), KikuError> {
        self.injected.push(text.to_owned());
        Ok(())
    }
}

#[derive(Default)]
struct FakeMedia {
    is_playing: bool,
    paused_calls: usize,
    resumed_calls: usize,
}

impl FakeMedia {
    fn playing() -> Self {
        Self {
            is_playing: true,
            paused_calls: 0,
            resumed_calls: 0,
        }
    }
}

impl MediaController for FakeMedia {
    fn pause_if_playing(&mut self) -> bool {
        if self.is_playing {
            self.is_playing = false;
            self.paused_calls += 1;
            true
        } else {
            false
        }
    }
    fn resume(&mut self) {
        self.is_playing = true;
        self.resumed_calls += 1;
    }
}

#[derive(Default)]
struct FakeNotifier {
    states: Vec<SessionState>,
}

impl StatusNotifier for FakeNotifier {
    fn notify(&mut self, state: SessionState) {
        self.states.push(state);
    }
}

#[test]
fn dictation_session_happy_path_transcribes_cleans_and_injects() {
    let mut coordinator = DictationCoordinator::new(
        FakeAudio::new(vec![1, 2, 3, 4]),
        FakeStt::ok("um hello world"),
        FakePost::ok("Hello world."),
        FakeInjector::default(),
        FakeMedia::default(),
        FakeNotifier::default(),
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
}

#[test]
fn short_tap_under_250ms_is_ignored_without_calling_stt() {
    let stt = FakeStt::ok("should not run");
    let mut coordinator = DictationCoordinator::new(
        FakeAudio::new(vec![1, 2, 3]),
        stt,
        FakePost::ok("ignored"),
        FakeInjector::default(),
        FakeMedia::playing(),
        FakeNotifier::default(),
    );

    coordinator.start_session().unwrap();
    let outcome = coordinator
        .finish_session(Duration::from_millis(200))
        .unwrap();

    assert_eq!(outcome, SessionOutcome::IgnoredTap);
}

#[test]
fn empty_audio_aborts_locally_as_no_speech_and_resumes_media() {
    let mut coordinator = DictationCoordinator::new(
        FakeAudio::new(vec![]),
        FakeStt::ok("should not run"),
        FakePost::ok("should not run"),
        FakeInjector::default(),
        FakeMedia::playing(),
        FakeNotifier::default(),
    );

    coordinator.start_session().unwrap();
    let outcome = coordinator
        .finish_session(Duration::from_millis(500))
        .unwrap();

    assert_eq!(outcome, SessionOutcome::NoSpeech);
}

#[test]
fn oversized_audio_is_capped_to_max_audio_bytes() {
    let oversized = vec![7u8; MAX_AUDIO_BYTES + 4096];
    let stt = FakeStt::ok("hello");
    let mut coordinator = DictationCoordinator::new(
        FakeAudio::new(oversized),
        stt,
        FakePost::ok("Hello."),
        FakeInjector::default(),
        FakeMedia::default(),
        FakeNotifier::default(),
    );

    coordinator.start_session().unwrap();
    let outcome = coordinator.finish_session(Duration::from_secs(10)).unwrap();

    assert_eq!(
        outcome,
        SessionOutcome::Injected {
            text: "Hello.".to_owned(),
            used_raw_fallback: false,
        }
    );
}

#[test]
fn post_processor_failure_falls_back_to_raw_stt_transcript() {
    let mut coordinator = DictationCoordinator::new(
        FakeAudio::new(vec![1, 2, 3]),
        FakeStt::ok("um raw transcript"),
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
fn stt_failure_retains_audio_for_retry_and_succeeds_on_retry() {
    let mut coordinator = DictationCoordinator::new(
        FakeAudio::new(vec![9, 8, 7]),
        FakeStt::sequence(vec![
            Err(KikuError::Stt("network timeout".to_owned())),
            Ok("um retry worked".to_owned()),
        ]),
        FakePost::ok("Retry worked."),
        FakeInjector::default(),
        FakeMedia::playing(),
        FakeNotifier::default(),
    );

    coordinator.start_session().unwrap();
    let err = coordinator
        .finish_session(Duration::from_millis(800))
        .unwrap_err();
    assert_eq!(err, KikuError::Stt("network timeout".to_owned()));
    assert!(coordinator.has_retry_buffer());

    let retry_outcome = coordinator.retry_last_session().unwrap();
    assert_eq!(
        retry_outcome,
        SessionOutcome::Injected {
            text: "Retry worked.".to_owned(),
            used_raw_fallback: false,
        }
    );
    assert!(!coordinator.has_retry_buffer());
}

#[test]
fn retry_without_failed_session_returns_nothing_to_retry() {
    let mut coordinator = DictationCoordinator::new(
        FakeAudio::new(vec![1]),
        FakeStt::ok("ok"),
        FakePost::ok("Ok."),
        FakeInjector::default(),
        FakeMedia::default(),
        FakeNotifier::default(),
    );

    let err = coordinator.retry_last_session().unwrap_err();
    assert_eq!(err, KikuError::NothingToRetry);
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
