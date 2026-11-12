use std::sync::{Arc, Mutex};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{SampleFormat, Stream};

use crate::ports::{
    AudioInput, CapturedAudio, KikuError, MAX_AUDIO_BYTES, MAX_SESSION_DURATION, RawAudio,
};

/// In-memory sample accumulator enforcing the 5-minute / ~10 MB capture cap.
pub struct CaptureBuffer {
    pub samples: Vec<f32>,
    pub sample_rate: u32,
    pub channels: u16,
    max_samples: usize,
}

impl CaptureBuffer {
    pub fn new(sample_rate: u32, channels: u16) -> Self {
        let duration_cap_samples = (sample_rate as usize)
            * (channels as usize)
            * (MAX_SESSION_DURATION.as_secs() as usize);
        let bytes_cap_samples = MAX_AUDIO_BYTES / std::mem::size_of::<f32>();
        Self {
            samples: Vec::new(),
            sample_rate,
            channels,
            max_samples: duration_cap_samples.min(bytes_cap_samples),
        }
    }

    pub fn push_samples(&mut self, incoming: &[f32]) {
        let remaining = self.max_samples.saturating_sub(self.samples.len());
        if remaining == 0 {
            return;
        }
        let take = incoming.len().min(remaining);
        self.samples.extend_from_slice(&incoming[..take]);
    }

    pub fn is_full(&self) -> bool {
        self.samples.len() >= self.max_samples
    }
}

/// Live cross-platform microphone capture adapter using `cpal`.
pub struct CpalAudioInput {
    active: Option<ActiveCapture>,
}

struct ActiveCapture {
    stream: Stream,
    buffer: Arc<Mutex<CaptureBuffer>>,
}

impl Default for CpalAudioInput {
    fn default() -> Self {
        Self::new()
    }
}

impl CpalAudioInput {
    pub fn new() -> Self {
        Self { active: None }
    }
}

impl AudioInput for CpalAudioInput {
    fn start_capture(&mut self) -> Result<(), KikuError> {
        if self.active.is_some() {
            return Err(KikuError::AudioCapture(
                "microphone capture already active".to_owned(),
            ));
        }

        let host = cpal::default_host();
        let device = host
            .default_input_device()
            .ok_or_else(|| KikuError::AudioCapture("no default input device found".to_owned()))?;

        let supported_config = device
            .default_input_config()
            .map_err(|e| KikuError::AudioCapture(format!("failed to query mic config: {e}")))?;

        let sample_format = supported_config.sample_format();
        let config: cpal::StreamConfig = supported_config.into();
        let buffer = Arc::new(Mutex::new(CaptureBuffer::new(
            config.sample_rate,
            config.channels,
        )));

        let buf_ref = Arc::clone(&buffer);
        let err_fn = |err| {
            eprintln!("[kiku] cpal input stream error: {err}");
        };

        let stream = match sample_format {
            SampleFormat::F32 => device.build_input_stream(
                config,
                move |data: &[f32], _| {
                    if let Ok(mut guard) = buf_ref.lock() {
                        guard.push_samples(data);
                    }
                },
                err_fn,
                None,
            ),
            SampleFormat::I16 => device.build_input_stream(
                config,
                move |data: &[i16], _| {
                    if let Ok(mut guard) = buf_ref.lock() {
                        let converted: Vec<f32> =
                            data.iter().map(|&s| f32::from(s) / 32768.0).collect();
                        guard.push_samples(&converted);
                    }
                },
                err_fn,
                None,
            ),
            SampleFormat::U16 => device.build_input_stream(
                config,
                move |data: &[u16], _| {
                    if let Ok(mut guard) = buf_ref.lock() {
                        let converted: Vec<f32> = data
                            .iter()
                            .map(|&s| (f32::from(s) - 32768.0) / 32768.0)
                            .collect();
                        guard.push_samples(&converted);
                    }
                },
                err_fn,
                None,
            ),
            other => {
                return Err(KikuError::AudioCapture(format!(
                    "unsupported microphone sample format: {other:?}"
                )));
            }
        }
        .map_err(|e| KikuError::AudioCapture(format!("failed to build mic stream: {e}")))?;

        stream
            .play()
            .map_err(|e| KikuError::AudioCapture(format!("failed to start mic stream: {e}")))?;

        self.active = Some(ActiveCapture { stream, buffer });
        Ok(())
    }

    fn stop_capture(&mut self) -> Result<CapturedAudio, KikuError> {
        let active = self.active.take().ok_or_else(|| {
            KikuError::AudioCapture("microphone capture is not active".to_owned())
        })?;
        drop(active.stream);

        let guard = active
            .buffer
            .lock()
            .map_err(|_| KikuError::AudioCapture("capture buffer lock poisoned".to_owned()))?;

        Ok(CapturedAudio::Raw(RawAudio {
            samples: guard.samples.clone(),
            sample_rate: guard.sample_rate,
            channels: guard.channels,
        }))
    }
}
