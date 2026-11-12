# Kiku

A fast, low-memory voice dictation desktop application that captures speech on key-hold, cleans audio, transcribes via cloud STT, removes filler words, and injects text at the active cursor.

## Language

**Dictation Session**:
A single end-to-end voice capture cycle triggered by pressing and holding the activation key until the cleaned transcript is injected or the cycle fails.
_Avoid_: Recording, transcription job, run

**Audio Pipeline**:
The in-memory preprocessing stages applied to raw microphone capture before cloud upload (resampling, noise suppression, silence trimming, and compression).
_Avoid_: Audio filter, preprocessor

**STT Provider**:
A cloud speech-to-text service that converts preprocessed audio into a raw transcript.
_Avoid_: Whisper client, transcriber

**Post-Processor**:
A cloud text-refinement step that removes filler words and cleans the raw transcript before injection.
_Avoid_: Formatter, LLM cleaner

**Text Injector**:
The mechanism that places the final transcript into the currently focused application at the user's cursor.
_Avoid_: Typer, paster

**Media Controller**:
The OS-level playback integration that pauses active media when a Dictation Session starts and resumes it when the session ends if kiku initiated the pause.
_Avoid_: Music pauser, audio manager

**Activation Key**:
The configurable key held down to start and sustain a Dictation Session and released to end capture.
_Avoid_: Hotkey, shortcut, trigger key
