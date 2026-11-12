use std::time::{Duration, Instant};

use kiku::config::KikuConfig;
use kiku::ports::{
    AudioBuffer, AudioFormat, AudioInput, KikuError, MAX_AUDIO_BYTES, MediaController,
    PostProcessor, STTProvider, SessionState, StatusNotifier, TextInjector,
};
use kiku::session::DictationCoordinator;

struct NoopAudio(AudioBuffer);
impl AudioInput for NoopAudio {
    fn start_capture(&mut self) -> Result<(), KikuError> {
        Ok(())
    }
    fn stop_capture(&mut self) -> Result<AudioBuffer, KikuError> {
        Ok(self.0.clone())
    }
}

struct NoopStt;
impl STTProvider for NoopStt {
    fn transcribe(&self, _audio: &AudioBuffer) -> Result<String, KikuError> {
        Ok("um hello world".to_owned())
    }
}

struct NoopPost;
impl PostProcessor for NoopPost {
    fn clean(&self, _raw: &str) -> Result<String, KikuError> {
        Ok("Hello world.".to_owned())
    }
}

struct NoopInjector;
impl TextInjector for NoopInjector {
    fn inject(&mut self, _text: &str) -> Result<(), KikuError> {
        Ok(())
    }
}

struct NoopMedia;
impl MediaController for NoopMedia {
    fn pause_if_playing(&mut self) -> bool {
        true
    }
    fn resume(&mut self) {}
}

struct NoopNotifier;
impl StatusNotifier for NoopNotifier {
    fn notify(&mut self, _state: SessionState) {}
}

#[test]
fn perf_dictation_orchestrator_1000_sessions_under_budget() {
    let mut coordinator = DictationCoordinator::new(
        NoopAudio(AudioBuffer {
            bytes: vec![0u8; 32_000],
            format: AudioFormat::Flac,
        }),
        NoopStt,
        NoopPost,
        NoopInjector,
        NoopMedia,
        NoopNotifier,
    );

    let iterations = 1_000;
    let start = Instant::now();
    for _ in 0..iterations {
        coordinator.start_session().unwrap();
        let _ = coordinator
            .finish_session(Duration::from_millis(500))
            .unwrap();
    }
    let elapsed = start.elapsed();
    let per_session_ns = elapsed.as_nanos() / iterations as u128;
    println!(
        "perf_dictation_orchestrator_1000_sessions: total={:?}, avg_per_session={}ns",
        elapsed, per_session_ns
    );
    assert!(
        elapsed < Duration::from_millis(500),
        "1000 sessions took too long: {elapsed:?}"
    );
}

#[test]
fn perf_max_audio_buffer_10mb_cap_under_budget() {
    let oversized = vec![42u8; MAX_AUDIO_BYTES + 1024];
    let mut coordinator = DictationCoordinator::new(
        NoopAudio(AudioBuffer {
            bytes: oversized,
            format: AudioFormat::Wav,
        }),
        NoopStt,
        NoopPost,
        NoopInjector,
        NoopMedia,
        NoopNotifier,
    );

    let start = Instant::now();
    coordinator.start_session().unwrap();
    let _ = coordinator
        .finish_session(Duration::from_secs(300))
        .unwrap();
    let elapsed = start.elapsed();
    println!("perf_max_audio_buffer_10mb_cap: elapsed={:?}", elapsed);
    assert!(
        elapsed < Duration::from_millis(250),
        "10MB buffer session took too long: {elapsed:?}"
    );
}

#[test]
fn perf_config_toml_parse_1000_iterations_under_budget() {
    let sample_toml = r#"
stt_model = "whisper-large-v3-turbo"
post_process_model = "llama-3.1-8b-instant"
activation_key = "RightOption"

[audio_pipeline]
resample_16khz_mono = true
noise_suppression = true
vad_trim = true
compress_opus_or_flac = true
"#;

    let start = Instant::now();
    for _ in 0..1_000 {
        let cfg: KikuConfig = toml::from_str(sample_toml).unwrap();
        assert_eq!(cfg.stt_model, "whisper-large-v3-turbo");
    }
    let elapsed = start.elapsed();
    println!(
        "perf_config_toml_parse_1000_iterations: total={:?}",
        elapsed
    );
    assert!(
        elapsed < Duration::from_millis(500),
        "1000 TOML parses took too long: {elapsed:?}"
    );
}
