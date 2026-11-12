# 0001. Use Rust for Core Engine and Thin OS Adapters

**Status**: accepted

We chose Rust for both the core dictation engine and thin macOS/Windows platform adapters instead of native Swift (macOS-only) or Go, and avoided WebView-based UI frameworks like Tauri. This gives us the strongest native audio/DSP ecosystem (`cpal`, `nnnoiseless`, `rubato`), keeps memory usage minimal (~10–20 MB) with instant startup, and supports both macOS and Windows from one codebase.
