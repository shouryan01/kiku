use crate::ports::MediaController;

/// Low-level OS playback backend abstracted for deterministic unit testing.
pub trait MediaBackend {
    fn pause_if_playing(&mut self) -> bool;
    fn resume(&mut self);
}

/// Tracks `paused_by_kiku` so resume is only issued when kiku initiated the pause.
pub struct ConditionalMediaController<B> {
    backend: B,
    paused_by_kiku: bool,
}

impl<B: MediaBackend> ConditionalMediaController<B> {
    pub fn new(backend: B) -> Self {
        Self {
            backend,
            paused_by_kiku: false,
        }
    }

    pub fn paused_by_kiku(&self) -> bool {
        self.paused_by_kiku
    }
}

impl<B: MediaBackend> MediaController for ConditionalMediaController<B> {
    fn pause_if_playing(&mut self) -> bool {
        let paused = self.backend.pause_if_playing();
        self.paused_by_kiku = paused;
        paused
    }

    fn resume(&mut self) {
        if std::mem::take(&mut self.paused_by_kiku) {
            self.backend.resume();
        }
    }
}

pub type OsMediaController = ConditionalMediaController<PlatformMediaBackend>;

impl Default for OsMediaController {
    fn default() -> Self {
        Self::new(PlatformMediaBackend::new())
    }
}

#[cfg(target_os = "windows")]
mod platform {
    use windows::Media::Control::{
        GlobalSystemMediaTransportControlsSessionManager,
        GlobalSystemMediaTransportControlsSessionPlaybackStatus,
    };

    use super::MediaBackend;

    #[derive(Default)]
    pub struct PlatformMediaBackend;

    impl PlatformMediaBackend {
        pub fn new() -> Self {
            Self
        }
    }

    impl MediaBackend for PlatformMediaBackend {
        fn pause_if_playing(&mut self) -> bool {
            let Ok(op) = GlobalSystemMediaTransportControlsSessionManager::RequestAsync() else {
                return false;
            };
            let Ok(manager) = op.join() else {
                return false;
            };
            let Ok(session) = manager.GetCurrentSession() else {
                return false;
            };
            let Ok(info) = session.GetPlaybackInfo() else {
                return false;
            };
            let Ok(status) = info.PlaybackStatus() else {
                return false;
            };
            if status != GlobalSystemMediaTransportControlsSessionPlaybackStatus::Playing {
                return false;
            }
            session
                .TryPauseAsync()
                .and_then(|op| op.join())
                .unwrap_or(false)
        }

        fn resume(&mut self) {
            let Ok(op) = GlobalSystemMediaTransportControlsSessionManager::RequestAsync() else {
                return;
            };
            let Ok(manager) = op.join() else {
                return;
            };
            let Ok(session) = manager.GetCurrentSession() else {
                return;
            };
            let _ = session.TryPlayAsync().and_then(|op| op.join());
        }
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use std::process::Command;

    use super::MediaBackend;

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum MacMediaTarget {
        Spotify,
        Music,
        NowPlayingCli,
    }

    #[derive(Default)]
    pub struct PlatformMediaBackend {
        last_target: Option<MacMediaTarget>,
    }

    impl PlatformMediaBackend {
        pub fn new() -> Self {
            Self { last_target: None }
        }
    }

    fn run_osascript(script: &str) -> Option<String> {
        let out = Command::new("osascript")
            .arg("-e")
            .arg(script)
            .output()
            .ok()?;
        if !out.status.success() {
            return None;
        }
        Some(String::from_utf8_lossy(&out.stdout).trim().to_owned())
    }

    fn is_app_playing(app: &str) -> bool {
        let script = format!(
            "if application \"{app}\" is running then tell application \"{app}\" to return (player state is playing) as string"
        );
        matches!(run_osascript(&script).as_deref(), Some("true"))
    }

    fn control_app(app: &str, cmd: &str) -> bool {
        let script = format!("tell application \"{app}\" to {cmd}");
        run_osascript(&script).is_some()
    }

    impl MediaBackend for PlatformMediaBackend {
        fn pause_if_playing(&mut self) -> bool {
            for (app, target) in [
                ("Spotify", MacMediaTarget::Spotify),
                ("Music", MacMediaTarget::Music),
            ] {
                if is_app_playing(app) && control_app(app, "pause") {
                    self.last_target = Some(target);
                    return true;
                }
            }

            if let Ok(out) = Command::new("nowplaying-cli")
                .args(["get", "playbackRate"])
                .output()
                && out.status.success()
                && String::from_utf8_lossy(&out.stdout).trim() == "1"
                && Command::new("nowplaying-cli")
                    .arg("pause")
                    .status()
                    .is_ok_and(|s| s.success())
            {
                self.last_target = Some(MacMediaTarget::NowPlayingCli);
                return true;
            }

            self.last_target = None;
            false
        }

        fn resume(&mut self) {
            match self.last_target.take() {
                Some(MacMediaTarget::Spotify) => {
                    let _ = control_app("Spotify", "play");
                }
                Some(MacMediaTarget::Music) => {
                    let _ = control_app("Music", "play");
                }
                Some(MacMediaTarget::NowPlayingCli) => {
                    let _ = Command::new("nowplaying-cli").arg("play").status();
                }
                None => {}
            }
        }
    }
}

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
mod platform {
    use super::MediaBackend;

    #[derive(Default)]
    pub struct PlatformMediaBackend;

    impl PlatformMediaBackend {
        pub fn new() -> Self {
            Self
        }
    }

    impl MediaBackend for PlatformMediaBackend {
        fn pause_if_playing(&mut self) -> bool {
            false
        }

        fn resume(&mut self) {}
    }
}

pub use platform::PlatformMediaBackend;
