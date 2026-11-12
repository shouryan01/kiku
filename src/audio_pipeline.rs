use earshot::Detector;
use flacenc::bitsink::ByteSink;
use flacenc::component::BitRepr;
use flacenc::error::Verify;
use nnnoiseless::DenoiseState;
use rubato::audioadapter_buffers::direct::SequentialSliceOfVecs;
use rubato::{Fft, FixedSync, Resampler};

use crate::config::AudioPipelineConfig;
use crate::ports::{AudioBuffer, AudioFormat, KikuError, RawAudio};

const TARGET_SAMPLE_RATE: u32 = 16_000;
const RNNOISE_SAMPLE_RATE: u32 = 48_000;
const RNNOISE_FRAME_SIZE: usize = DenoiseState::FRAME_SIZE; // 480 samples (10ms @ 48kHz)
const VAD_FRAME_SIZE: usize = 256; // 16ms @ 16kHz
const VAD_SPEECH_THRESHOLD: f32 = 0.5;
const VAD_PAD_FRAMES: usize = 6; // ~96ms padding before/after speech

/// Result of running raw microphone PCM through the 4-stage Audio Pipeline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PipelineOutput {
    NoSpeech,
    Ready(AudioBuffer),
}

/// Pure in-memory 4-stage Audio Pipeline:
/// 1. Resample to 16 kHz mono
/// 2. RNNoise denoising + RMS/peak normalization
/// 3. VAD silence trimming (with NoSpeech detection)
/// 4. In-memory FLAC compression (or WAV when disabled)
pub struct AudioPipeline {
    config: AudioPipelineConfig,
}

impl AudioPipeline {
    pub fn new(config: AudioPipelineConfig) -> Self {
        Self { config }
    }

    pub fn process(&self, raw: &RawAudio) -> Result<PipelineOutput, KikuError> {
        if raw.samples.is_empty() || raw.sample_rate == 0 || raw.channels == 0 {
            return Ok(PipelineOutput::NoSpeech);
        }

        let mut mono = downmix_to_mono(&raw.samples, raw.channels);
        let mut current_rate = raw.sample_rate;

        if self.config.noise_suppression {
            if current_rate != RNNOISE_SAMPLE_RATE {
                mono = resample_mono(&mono, current_rate, RNNOISE_SAMPLE_RATE)?;
                current_rate = RNNOISE_SAMPLE_RATE;
            }
            mono = denoise_rnnoise(&mono);
            normalize_rms_and_peak(&mut mono);
        }

        if self.config.resample_16khz_mono && current_rate != TARGET_SAMPLE_RATE {
            mono = resample_mono(&mono, current_rate, TARGET_SAMPLE_RATE)?;
            current_rate = TARGET_SAMPLE_RATE;
        }

        if self.config.vad_trim && current_rate == TARGET_SAMPLE_RATE {
            match trim_silence_vad(&mono) {
                Some(trimmed) => mono = trimmed,
                None => return Ok(PipelineOutput::NoSpeech),
            }
        }

        if mono.is_empty() {
            return Ok(PipelineOutput::NoSpeech);
        }

        let buffer = if self.config.compress_opus_or_flac {
            encode_flac(&mono, current_rate)?
        } else {
            encode_wav(&mono, current_rate)
        };

        Ok(PipelineOutput::Ready(buffer))
    }
}

fn downmix_to_mono(samples: &[f32], channels: u16) -> Vec<f32> {
    if channels == 1 {
        return samples.to_vec();
    }
    let ch = channels as usize;
    let inv = 1.0 / ch as f32;
    samples
        .chunks_exact(ch)
        .map(|frame| frame.iter().sum::<f32>() * inv)
        .collect()
}

fn resample_mono(input: &[f32], in_rate: u32, out_rate: u32) -> Result<Vec<f32>, KikuError> {
    if in_rate == out_rate || input.is_empty() {
        return Ok(input.to_vec());
    }
    let chunk_size = 1024usize.min(input.len()).max(64);
    let mut resampler = Fft::<f32>::new(
        in_rate as usize,
        out_rate as usize,
        chunk_size,
        1,
        FixedSync::Both,
    )
    .map_err(|e| KikuError::AudioPipeline(format!("resampler init failed: {e}")))?;

    let in_vecs = [input.to_vec()];
    let adapter = SequentialSliceOfVecs::new(&in_vecs, 1, input.len())
        .map_err(|e| KikuError::AudioPipeline(format!("resampler adapter failed: {e}")))?;
    let out = resampler
        .process_all(&adapter, input.len(), None)
        .map_err(|e| KikuError::AudioPipeline(format!("resampling failed: {e}")))?;
    Ok(out.take_data())
}

fn denoise_rnnoise(samples_48k: &[f32]) -> Vec<f32> {
    const SCALE: f32 = 32768.0;
    let mut state = DenoiseState::new();
    let mut out = Vec::with_capacity(samples_48k.len());
    let mut in_frame = [0.0f32; RNNOISE_FRAME_SIZE];
    let mut out_frame = [0.0f32; RNNOISE_FRAME_SIZE];

    for chunk in samples_48k.chunks(RNNOISE_FRAME_SIZE) {
        in_frame.fill(0.0);
        for (dst, &src) in in_frame.iter_mut().zip(chunk.iter()) {
            *dst = (src * SCALE).clamp(-32768.0, 32767.0);
        }
        state.process_frame(&mut out_frame, &in_frame);
        for &s in out_frame.iter().take(chunk.len()) {
            out.push((s / SCALE).clamp(-1.0, 1.0));
        }
    }
    out
}

fn normalize_rms_and_peak(samples: &mut [f32]) {
    if samples.is_empty() {
        return;
    }
    let sum_sq: f32 = samples.iter().map(|&s| s * s).sum();
    let rms = (sum_sq / samples.len() as f32).sqrt();
    if rms < 1e-6 {
        return;
    }
    let peak = samples.iter().fold(0.0f32, |acc, &s| acc.max(s.abs()));
    let target_rms = 0.1;
    let max_peak = 0.95;
    let rms_gain = (target_rms / rms).min(8.0);
    let peak_cap_gain = if peak > 0.0 { max_peak / peak } else { 1.0 };
    let gain = rms_gain.min(peak_cap_gain);
    if (gain - 1.0).abs() > 0.05 {
        for s in samples.iter_mut() {
            *s = (*s * gain).clamp(-1.0, 1.0);
        }
    }
}

fn trim_silence_vad(samples_16k: &[f32]) -> Option<Vec<f32>> {
    let mut detector = Detector::default_boxed();
    let total_frames = samples_16k.len() / VAD_FRAME_SIZE;
    if total_frames == 0 {
        return None;
    }

    let mut first_speech: Option<usize> = None;
    let mut last_speech: Option<usize> = None;

    let (frames, _) = samples_16k.as_chunks::<VAD_FRAME_SIZE>();
    for (idx, frame) in frames.iter().enumerate() {
        let mut buf = *frame;
        for s in &mut buf {
            *s = s.clamp(-1.0, 1.0);
        }
        if detector.predict_f32(&buf) >= VAD_SPEECH_THRESHOLD {
            if first_speech.is_none() {
                first_speech = Some(idx);
            }
            last_speech = Some(idx);
        }
    }

    let (start_frame, end_frame) = (first_speech?, last_speech?);
    let padded_start = start_frame.saturating_sub(VAD_PAD_FRAMES) * VAD_FRAME_SIZE;
    let padded_end = ((end_frame + 1 + VAD_PAD_FRAMES).min(total_frames) * VAD_FRAME_SIZE)
        .min(samples_16k.len());

    Some(samples_16k[padded_start..padded_end].to_vec())
}

fn encode_flac(samples: &[f32], sample_rate: u32) -> Result<AudioBuffer, KikuError> {
    let pcm_i32: Vec<i32> = samples
        .iter()
        .map(|&s| (s.clamp(-1.0, 1.0) * 32767.0).round() as i32)
        .collect();

    let config = flacenc::config::Encoder::default()
        .into_verified()
        .map_err(|e| KikuError::AudioPipeline(format!("invalid FLAC config: {e:?}")))?;

    let source = flacenc::source::MemSource::from_samples(&pcm_i32, 1, 16, sample_rate as usize);
    let stream = flacenc::encode_with_fixed_block_size(&config, source, config.block_size)
        .map_err(|e| KikuError::AudioPipeline(format!("FLAC encode failed: {e:?}")))?;

    let mut sink = ByteSink::new();
    stream
        .write(&mut sink)
        .map_err(|e| KikuError::AudioPipeline(format!("FLAC bitstream write failed: {e:?}")))?;

    Ok(AudioBuffer {
        bytes: sink.as_slice().to_vec(),
        format: AudioFormat::Flac,
    })
}

fn encode_wav(samples: &[f32], sample_rate: u32) -> AudioBuffer {
    let data_bytes = (samples.len() * 2) as u32;
    let mut bytes = Vec::with_capacity(44 + data_bytes as usize);
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&(36 + data_bytes).to_le_bytes());
    bytes.extend_from_slice(b"WAVEfmt ");
    bytes.extend_from_slice(&16u32.to_le_bytes()); // PCM chunk size
    bytes.extend_from_slice(&1u16.to_le_bytes()); // PCM format
    bytes.extend_from_slice(&1u16.to_le_bytes()); // mono
    bytes.extend_from_slice(&sample_rate.to_le_bytes());
    bytes.extend_from_slice(&(sample_rate * 2).to_le_bytes()); // byte rate
    bytes.extend_from_slice(&2u16.to_le_bytes()); // block align
    bytes.extend_from_slice(&16u16.to_le_bytes()); // bits per sample
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&data_bytes.to_le_bytes());
    for &s in samples {
        let sample_i16 = (s.clamp(-1.0, 1.0) * 32767.0).round() as i16;
        bytes.extend_from_slice(&sample_i16.to_le_bytes());
    }
    AudioBuffer {
        bytes,
        format: AudioFormat::Wav,
    }
}
