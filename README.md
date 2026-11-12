<p align="center">
  <img src="assets/kiku-logo.png" alt="kiku logo" width="160" />
</p>

# kiku

A fast, ultra-low-RAM voice dictation desktop app written in Rust (macOS primary, Windows supported). Press and hold a key to dictate, automatically pause background music, clean audio in memory, transcribe via free-tier cloud STT, strip filler words, and paste directly at your cursor.

## Features

- **Low-RAM Rust Core**: No WebView/Electron runtime; built for ~10–20 MB idle RAM and instant startup.
- **Press-and-Hold Dictation Session**: Ignores accidental taps (`<250ms`) and caps capture at 5 minutes (`~10 MB`).
- **Free-Tier Cloud Pipeline**:
  - **STT Provider**: Groq Whisper (`whisper-large-v3-turbo`)
  - **Post-Processor (Clean mode)**: Groq Llama (`llama-3.1-8b-instant`) removes filler words ("um", "uh", "like") and false starts while preserving your exact wording, with automatic fallback to the raw transcript on LLM failure.
- **Resilient Retry**: Retains failed STT audio in memory until the next session so transient network errors don't lose your speech.
- **TOML Config**: Stored at `~/.config/kiku/config.toml` (macOS/Linux) or `%APPDATA%\kiku\config.toml` (Windows), with `GROQ_API_KEY` resolved from env var first.

## Quickstart

### Prerequisites
- [Rust](https://rustup.rs/) (tested on `1.99.0`, Edition 2024)
- A free [Groq API key](https://console.groq.com/)

### Setup & Smoke Test

```bash
# 1. Set your Groq API key
export GROQ_API_KEY="gsk_..."   # PowerShell: $env:GROQ_API_KEY="gsk_..."

# 2. Initialize default TOML config
cargo run -- init-config

# 3. Inspect config path
cargo run -- config-path

# 4. Run end-to-end smoke test on a sample audio file (.wav, .flac, .ogg)
cargo run -- smoke path/to/sample.wav
```

### Run Tests

```bash
# Run all unit, integration, and performance tests
cargo test -- --nocapture

# Lint
cargo clippy --all-targets -- -D warnings
```

## Performance Test Results

Measured on Windows (`rustc 1.99.0`, unoptimized `dev`/`test` profile) via `cargo test --test performance -- --nocapture`:

| Benchmark Test | Observed Result | Budget |
|---|---|---|
| `perf_dictation_orchestrator_1000_sessions_under_budget` | **923.1 µs total** (~**923 ns/session**) | `< 500 ms` |
| `perf_max_audio_buffer_10mb_cap_under_budget` | **1.741 ms** (10 MB buffer cap & handoff) | `< 250 ms` |
| `perf_config_toml_parse_1000_iterations_under_budget` | **19.6307 ms total** (~19.6 µs/parse) | `< 500 ms` |
