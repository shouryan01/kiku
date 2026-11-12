use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::Duration;

use kiku::adapters::activation_key::{KeyEdge, run_hold_loop_with_control};
use kiku::adapters::tray::{
    TRAY_ICON_SIZE, TrayCommand, TrayStateModel, TrayStatusNotifier, handle_tray_command,
    render_state_icon_rgba,
};
use kiku::ports::{
    AudioBuffer, AudioFormat, AudioInput, CapturedAudio, KikuError, MediaController, PostProcessor,
    STTProvider, SessionState, TextInjector,
};
use kiku::session::DictationCoordinator;

struct StubAudio;
impl AudioInput for StubAudio {
    fn start_capture(&mut self) -> Result<(), KikuError> {
        Ok(())
    }

    fn stop_capture(&mut self) -> Result<CapturedAudio, KikuError> {
        Ok(CapturedAudio::Encoded(AudioBuffer {
            bytes: vec![1, 2, 3, 4],
            format: AudioFormat::Flac,
        }))
    }
}

struct FlakyStt {
    calls: Arc<AtomicUsize>,
}

impl STTProvider for FlakyStt {
    fn transcribe(&self, _audio: &AudioBuffer) -> Result<String, KikuError> {
        let n = self.calls.fetch_add(1, Ordering::SeqCst);
        if n == 0 {
            Err(KikuError::Stt("transient network error".to_owned()))
        } else {
            Ok("um hello tray retry".to_owned())
        }
    }
}

struct StubPost;
impl PostProcessor for StubPost {
    fn clean(&self, raw: &str) -> Result<String, KikuError> {
        Ok(raw.replace("um ", ""))
    }
}

struct RecordingInjector {
    injected: Arc<Mutex<Vec<String>>>,
}

impl TextInjector for RecordingInjector {
    fn inject(&mut self, text: &str) -> Result<(), KikuError> {
        self.injected.lock().unwrap().push(text.to_owned());
        Ok(())
    }
}

struct NoopMedia;
impl MediaController for NoopMedia {
    fn pause_if_playing(&mut self) -> bool {
        false
    }
    fn resume(&mut self) {}
}

#[test]
fn tray_state_model_updates_labels_tooltips_and_retry_enabled() {
    let mut model = TrayStateModel::default();
    assert_eq!(model.state, SessionState::Idle);
    assert!(!model.retry_enabled);
    assert_eq!(model.status_label(), "Status: Idle");

    model.apply(SessionState::Recording);
    assert!(model.status_label().contains("Recording"));
    assert!(model.tooltip().contains("Recording"));
    assert!(!model.retry_enabled);

    model.apply(SessionState::Processing);
    assert!(model.status_label().contains("Processing"));
    assert!(!model.retry_enabled);

    model.apply(SessionState::RetryAvailable);
    assert_eq!(model.status_label(), "Status: Retry Available");
    assert!(model.retry_enabled);

    model.apply(SessionState::Error);
    assert_eq!(model.status_label(), "Status: Error");
    assert!(!model.retry_enabled);

    model.apply(SessionState::Idle);
    assert!(!model.retry_enabled);
}

#[test]
fn render_state_icon_rgba_produces_distinct_16x16_icons() {
    let states = [
        SessionState::Idle,
        SessionState::Recording,
        SessionState::Processing,
        SessionState::Error,
        SessionState::RetryAvailable,
    ];
    let expected_len = (TRAY_ICON_SIZE * TRAY_ICON_SIZE * 4) as usize;
    let icons: Vec<Vec<u8>> = states.iter().map(|s| render_state_icon_rgba(*s)).collect();

    for icon in &icons {
        assert_eq!(icon.len(), expected_len);
        assert!(icon.iter().any(|&b| b != 0));
    }

    for i in 0..icons.len() {
        for j in (i + 1)..icons.len() {
            assert_ne!(
                icons[i], icons[j],
                "icons for {:?} and {:?} must differ",
                states[i], states[j]
            );
        }
    }
}

#[test]
fn tray_status_notifier_and_retry_command_rerun_failed_stt_session() {
    let (state_tx, state_rx) = mpsc::channel();
    let (key_tx, key_rx) = mpsc::channel();
    let (control_tx, control_rx) = mpsc::channel();

    let stt_calls = Arc::new(AtomicUsize::new(0));
    let injected = Arc::new(Mutex::new(Vec::new()));

    let stt_calls_worker = Arc::clone(&stt_calls);
    let injected_worker = Arc::clone(&injected);

    let worker = thread::spawn(move || {
        let mut coordinator = DictationCoordinator::new(
            StubAudio,
            FlakyStt {
                calls: stt_calls_worker,
            },
            StubPost,
            RecordingInjector {
                injected: injected_worker,
            },
            NoopMedia,
            TrayStatusNotifier::new(state_tx),
        );
        run_hold_loop_with_control(&mut coordinator, &key_rx, Some(&control_rx));
    });

    // Hold >= 250ms so session starts and then fails at STT.
    key_tx.send(KeyEdge::Down).unwrap();
    thread::sleep(Duration::from_millis(310));
    key_tx.send(KeyEdge::Up).unwrap();

    // Wait for RetryAvailable notification from TrayStatusNotifier.
    let mut model = TrayStateModel::default();
    let mut observed = Vec::new();
    for _ in 0..50 {
        while let Ok(state) = state_rx.try_recv() {
            model.apply(state);
            observed.push(state);
        }
        if model.retry_enabled {
            break;
        }
        thread::sleep(Duration::from_millis(20));
    }
    assert!(
        model.retry_enabled,
        "expected RetryAvailable, observed: {observed:?}"
    );
    assert!(injected.lock().unwrap().is_empty());

    // Trigger Retry Last Dictation from the tray command handler.
    let dummy_config = PathBuf::from("unused.toml");
    let should_quit =
        handle_tray_command(TrayCommand::RetryLastDictation, &control_tx, &dummy_config);
    assert!(!should_quit);

    // Wait for Idle after successful retry.
    for _ in 0..50 {
        while let Ok(state) = state_rx.try_recv() {
            model.apply(state);
            observed.push(state);
        }
        if model.state == SessionState::Idle && !injected.lock().unwrap().is_empty() {
            break;
        }
        thread::sleep(Duration::from_millis(20));
    }

    assert_eq!(stt_calls.load(Ordering::SeqCst), 2);
    assert_eq!(injected.lock().unwrap().as_slice(), &["hello tray retry"]);
    assert!(!model.retry_enabled);

    // Trigger Quit and verify clean worker shutdown.
    let should_quit = handle_tray_command(TrayCommand::Quit, &control_tx, &dummy_config);
    assert!(should_quit);
    worker.join().expect("worker should exit cleanly on Quit");
}
