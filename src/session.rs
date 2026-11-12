use std::time::Duration;

use crate::ports::{
    AudioBuffer, AudioInput, KikuError, MAX_AUDIO_BYTES, MAX_SESSION_DURATION, MIN_HOLD_DURATION,
    MediaController, PostProcessor, STTProvider, SessionOutcome, SessionState, StatusNotifier,
    TextInjector,
};

/// Coordinates the lifecycle of a single Dictation Session across platform and cloud ports.
pub struct DictationCoordinator<A, S, P, T, M, N> {
    audio: A,
    stt: S,
    post_processor: P,
    injector: T,
    media: M,
    notifier: N,
    retry_buffer: Option<AudioBuffer>,
    active_session: Option<ActiveSession>,
}

struct ActiveSession {
    paused_by_kiku: bool,
}

impl<A, S, P, T, M, N> DictationCoordinator<A, S, P, T, M, N>
where
    A: AudioInput,
    S: STTProvider,
    P: PostProcessor,
    T: TextInjector,
    M: MediaController,
    N: StatusNotifier,
{
    pub fn new(audio: A, stt: S, post_processor: P, injector: T, media: M, notifier: N) -> Self {
        Self {
            audio,
            stt,
            post_processor,
            injector,
            media,
            notifier,
            retry_buffer: None,
            active_session: None,
        }
    }

    pub fn has_retry_buffer(&self) -> bool {
        self.retry_buffer.is_some()
    }

    pub fn start_session(&mut self) -> Result<(), KikuError> {
        if self.active_session.is_some() {
            return Err(KikuError::SessionAlreadyActive);
        }

        let paused_by_kiku = self.media.pause_if_playing();
        if let Err(err) = self.audio.start_capture() {
            if paused_by_kiku {
                self.media.resume();
            }
            self.notifier.notify(SessionState::Error);
            return Err(err);
        }

        self.active_session = Some(ActiveSession { paused_by_kiku });
        self.notifier.notify(SessionState::Recording);
        Ok(())
    }

    pub fn finish_session(&mut self, hold_duration: Duration) -> Result<SessionOutcome, KikuError> {
        let session = self
            .active_session
            .take()
            .ok_or(KikuError::NoActiveSession)?;

        let mut buffer = match self.audio.stop_capture() {
            Ok(buf) => buf,
            Err(err) => {
                self.resume_if_paused(session.paused_by_kiku);
                self.notifier.notify(SessionState::Error);
                return Err(err);
            }
        };

        if hold_duration < MIN_HOLD_DURATION {
            self.resume_if_paused(session.paused_by_kiku);
            self.notifier.notify(SessionState::Idle);
            return Ok(SessionOutcome::IgnoredTap);
        }

        if buffer.bytes.len() > MAX_AUDIO_BYTES {
            buffer.bytes.truncate(MAX_AUDIO_BYTES);
        }

        if buffer.bytes.is_empty() {
            self.resume_if_paused(session.paused_by_kiku);
            self.notifier.notify(SessionState::Idle);
            return Ok(SessionOutcome::NoSpeech);
        }

        // Enforce duration invariant awareness for callers (capture is already stopped).
        let _ = hold_duration.min(MAX_SESSION_DURATION);

        // New valid capture replaces any previous failed retry buffer.
        self.retry_buffer = None;
        self.notifier.notify(SessionState::Processing);
        self.process_audio(buffer, session.paused_by_kiku)
    }

    pub fn retry_last_session(&mut self) -> Result<SessionOutcome, KikuError> {
        let buffer = self.retry_buffer.take().ok_or(KikuError::NothingToRetry)?;

        self.notifier.notify(SessionState::Processing);
        self.process_audio(buffer, false)
    }

    fn process_audio(
        &mut self,
        buffer: AudioBuffer,
        paused_by_kiku: bool,
    ) -> Result<SessionOutcome, KikuError> {
        let raw_transcript = match self.stt.transcribe(&buffer) {
            Ok(text) => text,
            Err(err) => {
                self.retry_buffer = Some(buffer);
                self.resume_if_paused(paused_by_kiku);
                self.notifier.notify(SessionState::RetryAvailable);
                return Err(err);
            }
        };

        let (final_text, used_raw_fallback) = match self.post_processor.clean(&raw_transcript) {
            Ok(cleaned) => (cleaned, false),
            Err(_) => (raw_transcript, true),
        };

        if let Err(err) = self.injector.inject(&final_text) {
            self.resume_if_paused(paused_by_kiku);
            self.notifier.notify(SessionState::Error);
            return Err(err);
        }

        self.resume_if_paused(paused_by_kiku);
        self.notifier.notify(SessionState::Idle);
        Ok(SessionOutcome::Injected {
            text: final_text,
            used_raw_fallback,
        })
    }

    fn resume_if_paused(&mut self, paused_by_kiku: bool) {
        if paused_by_kiku {
            self.media.resume();
        }
    }
}
