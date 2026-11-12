use std::f32::consts::PI;

use kiku::audio_pipeline::{AudioPipeline, PipelineOutput};
use kiku::config::AudioPipelineConfig;
use kiku::ports::{AudioFormat, RawAudio};

/// Generates a 16 kHz-compatible speech-like harmonic complex (richer than a single sine so VAD triggers).
fn synth_speech_like_48k_stereo(
    lead_silence_secs: f32,
    speech_secs: f32,
    trail_silence_secs: f32,
) -> RawAudio {
    let rate = 48_000usize;
    let lead = (lead_silence_secs * rate as f32) as usize;
    let speech = (speech_secs * rate as f32) as usize;
    let trail = (trail_silence_secs * rate as f32) as usize;
    let total = lead + speech + trail;

    let mut samples = Vec::with_capacity(total * 2);
    for i in 0..total {
        let v = if i >= lead && i < lead + speech {
            let t = (i - lead) as f32 / rate as f32;
            // Formant-like voiced harmonic stack with slow amplitude envelope
            let env = (2.0 * PI * 3.0 * t).sin().abs().max(0.2);
            env * 0.25
                * ((2.0 * PI * 180.0 * t).sin()
                    + 0.7 * (2.0 * PI * 360.0 * t).sin()
                    + 0.5 * (2.0 * PI * 720.0 * t).sin()
                    + 0.3 * (2.0 * PI * 1400.0 * t).sin())
        } else {
            0.0
        };
        samples.push(v);
        samples.push(v);
    }

    RawAudio {
        samples,
        sample_rate: rate as u32,
        channels: 2,
    }
}

#[test]
fn pure_silence_returns_no_speech() {
    let pipeline = AudioPipeline::new(AudioPipelineConfig::default());
    let silence = RawAudio {
        samples: vec![0.0; 48_000],
        sample_rate: 48_000,
        channels: 1,
    };

    let output = pipeline.process(&silence).unwrap();
    assert_eq!(output, PipelineOutput::NoSpeech);
}

#[test]
fn speech_with_silence_is_resampled_denoised_trimmed_and_flac_encoded() {
    let raw = synth_speech_like_48k_stereo(1.0, 1.0, 1.0);
    let pipeline = AudioPipeline::new(AudioPipelineConfig::default());

    let output = pipeline.process(&raw).unwrap();
    match output {
        PipelineOutput::Ready(buf) => {
            assert_eq!(buf.format, AudioFormat::Flac);
            assert!(buf.bytes.starts_with(b"fLaC"), "expected FLAC magic header");
            // 1s of trimmed speech in FLAC should be much smaller than 1s 16kHz WAV (32,044 bytes)
            // or 3s 48kHz stereo PCM (~1.15 MB).
            assert!(
                buf.bytes.len() < 40_000,
                "expected trimmed FLAC < 40KB, got {} bytes",
                buf.bytes.len()
            );
        }
        PipelineOutput::NoSpeech => panic!("expected speech to be detected"),
    }
}

#[test]
fn disabling_compression_outputs_16khz_mono_wav() {
    let raw = synth_speech_like_48k_stereo(0.2, 0.8, 0.2);
    let config = AudioPipelineConfig {
        compress_opus_or_flac: false,
        ..AudioPipelineConfig::default()
    };
    let pipeline = AudioPipeline::new(config);

    match pipeline.process(&raw).unwrap() {
        PipelineOutput::Ready(buf) => {
            assert_eq!(buf.format, AudioFormat::Wav);
            assert!(buf.bytes.starts_with(b"RIFF"));
            let channels = u16::from_le_bytes([buf.bytes[22], buf.bytes[23]]);
            let sample_rate =
                u32::from_le_bytes([buf.bytes[24], buf.bytes[25], buf.bytes[26], buf.bytes[27]]);
            assert_eq!(channels, 1);
            assert_eq!(sample_rate, 16_000);
        }
        PipelineOutput::NoSpeech => panic!("expected speech to be detected"),
    }
}

#[test]
fn disabling_vad_and_rnnoise_resamples_full_clip_to_16khz_mono_wav() {
    let silence = RawAudio {
        samples: vec![0.0; 48_000], // 1.0s @ 48kHz mono
        sample_rate: 48_000,
        channels: 1,
    };
    let config = AudioPipelineConfig {
        resample_16khz_mono: true,
        noise_suppression: false,
        vad_trim: false,
        compress_opus_or_flac: false,
    };
    let pipeline = AudioPipeline::new(config);

    match pipeline.process(&silence).unwrap() {
        PipelineOutput::Ready(buf) => {
            assert_eq!(buf.format, AudioFormat::Wav);
            // 1.0s @ 16kHz 16-bit mono = 32,000 bytes + 44 byte WAV header
            assert!(
                (31_500..=32_500).contains(&buf.bytes.len()),
                "expected ~32,044 bytes, got {}",
                buf.bytes.len()
            );
        }
        PipelineOutput::NoSpeech => panic!("expected NoSpeech only when VAD is enabled"),
    }
}

#[test]
fn vad_trims_leading_and_trailing_silence_from_wav_output() {
    // 1.5s silence + 0.8s speech + 1.5s silence = 3.8s total @ 48kHz stereo
    let raw = synth_speech_like_48k_stereo(1.5, 0.8, 1.5);
    let config = AudioPipelineConfig {
        compress_opus_or_flac: false,
        ..AudioPipelineConfig::default()
    };
    let pipeline = AudioPipeline::new(config);

    match pipeline.process(&raw).unwrap() {
        PipelineOutput::Ready(buf) => {
            assert_eq!(buf.format, AudioFormat::Wav);
            // Un-trimmed 3.8s @ 16kHz mono 16-bit WAV would be ~121,644 bytes.
            // Trimmed 0.8s + ~0.2s padding should be ~32,000 bytes (definitely < 55,000 bytes).
            assert!(
                buf.bytes.len() < 55_000,
                "expected trimmed WAV < 55KB, got {} bytes",
                buf.bytes.len()
            );
        }
        PipelineOutput::NoSpeech => panic!("expected speech to be detected"),
    }
}

#[test]
fn normalization_boosts_quiet_signal_amplitude() {
    // Very quiet speech-like signal (amplitude 0.01)
    let rate = 48_000usize;
    let mut samples = Vec::with_capacity(rate);
    for i in 0..rate {
        let t = i as f32 / rate as f32;
        samples.push(0.01 * ((2.0 * PI * 180.0 * t).sin() + 0.5 * (2.0 * PI * 360.0 * t).sin()));
    }
    let raw = RawAudio {
        samples,
        sample_rate: rate as u32,
        channels: 1,
    };

    let unnormalized = AudioPipeline::new(AudioPipelineConfig {
        noise_suppression: false,
        vad_trim: false,
        compress_opus_or_flac: false,
        resample_16khz_mono: true,
    })
    .process(&raw)
    .unwrap();

    let normalized = AudioPipeline::new(AudioPipelineConfig {
        noise_suppression: true,
        vad_trim: false,
        compress_opus_or_flac: false,
        resample_16khz_mono: true,
    })
    .process(&raw)
    .unwrap();

    let peak = |bytes: &[u8]| -> i16 {
        bytes[44..]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| i16::from_le_bytes(*c).saturating_abs())
            .max()
            .unwrap_or(0)
    };

    if let (PipelineOutput::Ready(raw_wav), PipelineOutput::Ready(norm_wav)) =
        (unnormalized, normalized)
    {
        assert!(
            peak(&norm_wav.bytes) > peak(&raw_wav.bytes) * 2,
            "expected normalization to boost quiet signal (raw peak={}, norm peak={})",
            peak(&raw_wav.bytes),
            peak(&norm_wav.bytes)
        );
    } else {
        panic!("expected Ready WAV outputs");
    }
}
