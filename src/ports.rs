use std::time::Duration;
use thiserror::Error;

/// Maximum duration of a single Dictation Session before capture stops automatically.
pub const MAX_SESSION_DURATION: Duration = Duration::from_secs(5 * 60);

/// Maximum in-memory audio buffer size (~10 MB) for a single Dictation Session.
pub const MAX_AUDIO_BYTES: usize = 10 * 1024 * 1024;

/// Minimum Activation Key hold duration; shorter holds are treated as accidental taps.
pub const MIN_HOLD_DURATION: Duration = Duration::from_millis(250);

/// Visual/audio state emitted to the StatusNotifier boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionState {
    Idle,
    Recording,
    Processing,
    Error,
    RetryAvailable,
}

/// Encoded audio container format sent to an STT Provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioFormat {
    Wav,
    Flac,
    OggOpus,
}

impl AudioFormat {
    pub fn filename(&self) -> &'static str {
        match self {
            Self::Wav => "audio.wav",
            Self::Flac => "audio.flac",
            Self::OggOpus => "audio.ogg",
        }
    }

    pub fn mime_type(&self) -> &'static str {
        match self {
            Self::Wav => "audio/wav",
            Self::Flac => "audio/flac",
            Self::OggOpus => "audio/ogg",
        }
    }
}

/// Captured or preprocessed audio payload for a Dictation Session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioBuffer {
    pub bytes: Vec<u8>,
    pub format: AudioFormat,
}

/// Outcome of finishing or retrying a Dictation Session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionOutcome {
    /// Hold was shorter than MIN_HOLD_DURATION and ignored.
    IgnoredTap,
    /// Audio Pipeline detected no speech; aborted locally.
    NoSpeech,
    /// Transcript was produced and injected (with whether Post-Processor fallback occurred).
    Injected {
        text: String,
        used_raw_fallback: bool,
    },
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum KikuError {
    #[error("configuration error: {0}")]
    Config(String),
    #[error("audio capture error: {0}")]
    AudioCapture(String),
    #[error("STT provider error: {0}")]
    Stt(String),
    #[error("post-processor error: {0}")]
    PostProcess(String),
    #[error("text injection error: {0}")]
    Injection(String),
    #[error("no failed session available to retry")]
    NothingToRetry,
    #[error("session is already active")]
    SessionAlreadyActive,
    #[error("no active session")]
    NoActiveSession,
}

/// Captures microphone audio for a Dictation Session.
pub trait AudioInput {
    fn start_capture(&mut self) -> Result<(), KikuError>;
    fn stop_capture(&mut self) -> Result<AudioBuffer, KikuError>;
}

/// Cloud speech-to-text provider.
pub trait STTProvider {
    fn transcribe(&self, audio: &AudioBuffer) -> Result<String, KikuError>;
}

/// Cloud post-processor that removes filler words and cleans raw transcripts.
pub trait PostProcessor {
    fn clean(&self, raw_transcript: &str) -> Result<String, KikuError>;
}

/// Injects final text at the active cursor.
pub trait TextInjector {
    fn inject(&mut self, text: &str) -> Result<(), KikuError>;
}

/// Pauses and conditionally resumes active OS media playback.
pub trait MediaController {
    /// Returns true if media was playing and is now paused.
    fn pause_if_playing(&mut self) -> bool;
    /// Resumes media playback when kiku previously initiated the pause.
    fn resume(&mut self);
}

/// Reports Dictation Session state transitions to the UI/tray layer.
pub trait StatusNotifier {
    fn notify(&mut self, state: SessionState);
}
