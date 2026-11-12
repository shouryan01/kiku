use std::env;
use std::io::{self, BufRead};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use kiku::adapters::activation_key::{
    ActivationKey, is_input_monitoring_trusted, run_hold_loop, run_hold_loop_with_control,
    spawn_key_listener,
};
use kiku::adapters::audio_cpal::CpalAudioInput;
use kiku::adapters::media_controller::OsMediaController;
use kiku::adapters::text_injector::{ClipboardTextInjector, is_accessibility_trusted};
use kiku::adapters::tray::{TrayStatusNotifier, run_tray_event_loop};
use kiku::config::KikuConfig;
use kiku::ports::{
    AudioBuffer, AudioFormat, AudioInput, CapturedAudio, KikuError, MediaController,
    SessionOutcome, SessionState, StatusNotifier, TextInjector,
};
use kiku::providers::groq::{GroqPostProcessor, GroqSttProvider};
use kiku::session::DictationCoordinator;

struct FileAudioInput {
    path: PathBuf,
}

impl AudioInput for FileAudioInput {
    fn start_capture(&mut self) -> Result<(), KikuError> {
        if !self.path.exists() {
            return Err(KikuError::AudioCapture(format!(
                "audio file not found: {}",
                self.path.display()
            )));
        }
        Ok(())
    }

    fn stop_capture(&mut self) -> Result<CapturedAudio, KikuError> {
        let bytes = std::fs::read(&self.path).map_err(|e| {
            KikuError::AudioCapture(format!("failed to read {}: {e}", self.path.display()))
        })?;
        let format = match self.path.extension().and_then(|e| e.to_str()) {
            Some("flac") => AudioFormat::Flac,
            Some("ogg") | Some("opus") => AudioFormat::OggOpus,
            _ => AudioFormat::Wav,
        };
        Ok(CapturedAudio::Encoded(AudioBuffer { bytes, format }))
    }
}

struct StdoutInjector;
impl TextInjector for StdoutInjector {
    fn inject(&mut self, text: &str) -> Result<(), KikuError> {
        println!("{text}");
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

struct StderrNotifier;
impl StatusNotifier for StderrNotifier {
    fn notify(&mut self, state: SessionState) {
        eprintln!("[kiku] state -> {state:?}");
    }
}

fn run() -> Result<(), KikuError> {
    let mut args = env::args().skip(1);
    match args.next().as_deref() {
        Some("config-path") => {
            let path = KikuConfig::default_path()?;
            println!("{}", path.display());
            Ok(())
        }
        Some("init-config") => {
            let path = KikuConfig::default_path()?;
            KikuConfig::write_default_if_missing(&path)?;
            println!("Config ready at {}", path.display());
            Ok(())
        }
        Some("smoke") => {
            let audio_path = args.next().ok_or_else(|| {
                KikuError::Config("usage: kiku smoke <path-to-audio-file>".to_owned())
            })?;
            let config_path = KikuConfig::default_path()?;
            let config = KikuConfig::load_from_path(&config_path)?;
            let api_key = config.resolve_groq_api_key()?;

            let stt = GroqSttProvider::new(api_key.clone(), config.stt_model)?;
            let post = GroqPostProcessor::new(api_key, config.post_process_model)?;

            let mut coordinator = DictationCoordinator::with_pipeline_config(
                FileAudioInput {
                    path: PathBuf::from(audio_path),
                },
                config.audio_pipeline,
                stt,
                post,
                StdoutInjector,
                NoopMedia,
                StderrNotifier,
            );

            coordinator.start_session()?;
            match coordinator.finish_session(Duration::from_millis(500))? {
                SessionOutcome::Injected {
                    used_raw_fallback, ..
                } if used_raw_fallback => {
                    eprintln!("[kiku] warning: post-processor failed, used raw STT transcript");
                }
                _ => {}
            }
            Ok(())
        }
        Some("dictate") => {
            if !is_accessibility_trusted() {
                return Err(KikuError::Injection(
                    "macOS Accessibility permission is required for pasting. Enable kiku in System Settings -> Privacy & Security -> Accessibility.".to_owned(),
                ));
            }
            let config_path = KikuConfig::default_path()?;
            let config = KikuConfig::load_from_path(&config_path)?;
            let api_key = config.resolve_groq_api_key()?;

            let stt = GroqSttProvider::new(api_key.clone(), config.stt_model)?;
            let post = GroqPostProcessor::new(api_key, config.post_process_model)?;
            let injector = ClipboardTextInjector::new_os()?;

            let mut coordinator = DictationCoordinator::with_pipeline_config(
                CpalAudioInput::new(),
                config.audio_pipeline,
                stt,
                post,
                injector,
                OsMediaController::default(),
                StderrNotifier,
            );

            eprintln!("[kiku] Recording from default microphone. Press Enter to stop and paste...");
            let started_at = Instant::now();
            coordinator.start_session()?;

            let mut line = String::new();
            let _ = io::stdin().lock().read_line(&mut line);

            match coordinator.finish_session(started_at.elapsed())? {
                SessionOutcome::IgnoredTap => {
                    eprintln!("[kiku] Hold was under 250ms; ignored.");
                }
                SessionOutcome::NoSpeech => {
                    eprintln!("[kiku] No speech detected; aborted locally.");
                }
                SessionOutcome::Injected {
                    used_raw_fallback, ..
                } if used_raw_fallback => {
                    eprintln!("[kiku] warning: post-processor failed, used raw STT transcript");
                }
                SessionOutcome::Injected { .. } => {}
            }
            Ok(())
        }
        Some("run") => {
            let headless = args.next().as_deref() == Some("--headless");
            if !is_input_monitoring_trusted() {
                return Err(KikuError::Config(
                    "macOS Input Monitoring permission is required. Enable kiku in System Settings -> Privacy & Security -> Input Monitoring.".to_owned(),
                ));
            }
            if !is_accessibility_trusted() {
                return Err(KikuError::Injection(
                    "macOS Accessibility permission is required for pasting. Enable kiku in System Settings -> Privacy & Security -> Accessibility.".to_owned(),
                ));
            }
            let config_path = KikuConfig::default_path()?;
            let config = KikuConfig::load_from_path(&config_path)?;
            let api_key = config.resolve_groq_api_key()?;
            let key = ActivationKey::parse(&config.activation_key)?;

            let stt = GroqSttProvider::new(api_key.clone(), config.stt_model)?;
            let post = GroqPostProcessor::new(api_key, config.post_process_model)?;
            let injector = ClipboardTextInjector::new_os()?;
            let events = spawn_key_listener(key)?;

            if headless {
                let mut coordinator = DictationCoordinator::with_pipeline_config(
                    CpalAudioInput::new(),
                    config.audio_pipeline,
                    stt,
                    post,
                    injector,
                    OsMediaController::default(),
                    StderrNotifier,
                );
                eprintln!(
                    "[kiku] Listening for press-and-hold on {:?}. Press Ctrl+C to quit.",
                    key
                );
                run_hold_loop(&mut coordinator, &events);
                return Ok(());
            }

            let (state_tx, state_rx) = mpsc::channel();
            let (control_tx, control_rx) = mpsc::channel();
            let pipeline_config = config.audio_pipeline;

            let worker = thread::spawn(move || {
                let mut coordinator = DictationCoordinator::with_pipeline_config(
                    CpalAudioInput::new(),
                    pipeline_config,
                    stt,
                    post,
                    injector,
                    OsMediaController::default(),
                    TrayStatusNotifier::new(state_tx),
                );
                run_hold_loop_with_control(&mut coordinator, &events, Some(&control_rx));
            });

            let tray_res = run_tray_event_loop(&state_rx, &control_tx, &config_path);
            let _ = worker.join();
            tray_res
        }
        _ => {
            eprintln!(
                "Usage:\n  kiku config-path\n  kiku init-config\n  kiku smoke <audio-file>\n  kiku dictate\n  kiku run [--headless]"
            );
            Ok(())
        }
    }
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("error: {err}");
            ExitCode::FAILURE
        }
    }
}
