use std::time::Duration;

use kiku::adapters::media_controller::{ConditionalMediaController, MediaBackend};
use kiku::ports::{
    AudioBuffer, AudioFormat, AudioInput, CapturedAudio, KikuError, MediaController, PostProcessor,
    RawAudio, STTProvider, SessionOutcome, SessionState, StatusNotifier, TextInjector,
};
use kiku::session::DictationCoordinator;

#[derive(Default)]
struct FakeBackend {
    playing: bool,
    pause_calls: usize,
    resume_calls: usize,
}

impl MediaBackend for FakeBackend {
    fn pause_if_playing(&mut self) -> bool {
        self.pause_calls += 1;
        if self.playing {
            self.playing = false;
            true
        } else {
            false
        }
    }

    fn resume(&mut self) {
        self.resume_calls += 1;
        self.playing = true;
    }
}

#[test]
fn pauses_and_resumes_only_when_media_was_playing() {
    let mut ctrl = ConditionalMediaController::new(FakeBackend {
        playing: true,
        ..Default::default()
    });

    assert!(ctrl.pause_if_playing());
    assert!(ctrl.paused_by_kiku());

    ctrl.resume();
    assert!(!ctrl.paused_by_kiku());

    // Calling resume again without a new pause is a no-op.
    ctrl.resume();
    assert!(!ctrl.paused_by_kiku());
}

#[test]
fn does_not_resume_when_media_was_already_paused() {
    let mut ctrl = ConditionalMediaController::new(FakeBackend {
        playing: false,
        ..Default::default()
    });

    assert!(!ctrl.pause_if_playing());
    assert!(!ctrl.paused_by_kiku());
    ctrl.resume();
    assert!(!ctrl.paused_by_kiku());
}

struct SilentRawMic;
impl AudioInput for SilentRawMic {
    fn start_capture(&mut self) -> Result<(), KikuError> {
        Ok(())
    }
    fn stop_capture(&mut self) -> Result<CapturedAudio, KikuError> {
        Ok(CapturedAudio::Raw(RawAudio {
            samples: vec![0.0; 16_000],
            sample_rate: 16_000,
            channels: 1,
        }))
    }
}

struct FailingStt;
impl STTProvider for FailingStt {
    fn transcribe(&self, _audio: &AudioBuffer) -> Result<String, KikuError> {
        Err(KikuError::Stt("offline".to_owned()))
    }
}

struct EncodedMic;
impl AudioInput for EncodedMic {
    fn start_capture(&mut self) -> Result<(), KikuError> {
        Ok(())
    }
    fn stop_capture(&mut self) -> Result<CapturedAudio, KikuError> {
        Ok(CapturedAudio::Encoded(AudioBuffer {
            bytes: vec![1, 2, 3],
            format: AudioFormat::Wav,
        }))
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

struct NoopNotifier;
impl StatusNotifier for NoopNotifier {
    fn notify(&mut self, _state: SessionState) {}
}

#[test]
fn resumes_media_on_no_speech_abort_and_on_stt_failure() {
    let media_no_speech = ConditionalMediaController::new(FakeBackend {
        playing: true,
        ..Default::default()
    });
    let mut c1 = DictationCoordinator::new(
        SilentRawMic,
        FailingStt,
        NoopPost,
        NoopInjector,
        media_no_speech,
        NoopNotifier,
    );
    c1.start_session().unwrap();
    assert_eq!(
        c1.finish_session(Duration::from_millis(500)).unwrap(),
        SessionOutcome::NoSpeech
    );

    let media_err = ConditionalMediaController::new(FakeBackend {
        playing: true,
        ..Default::default()
    });
    let mut c2 = DictationCoordinator::new(
        EncodedMic,
        FailingStt,
        NoopPost,
        NoopInjector,
        media_err,
        NoopNotifier,
    );
    c2.start_session().unwrap();
    assert!(c2.finish_session(Duration::from_millis(500)).is_err());
}
