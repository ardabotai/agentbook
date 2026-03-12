use crate::app::NotificationCue;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::OnceLock;

struct SoundAssets {
    room_join: PathBuf,
    room_leave: PathBuf,
    room_message: PathBuf,
}

static SOUND_ASSETS: OnceLock<Option<SoundAssets>> = OnceLock::new();

pub fn play_notification_cue(cue: NotificationCue) {
    match cue {
        NotificationCue::Generic => play_terminal_bell(),
        NotificationCue::RoomJoin => play_room_sound(|assets| &assets.room_join),
        NotificationCue::RoomLeave => play_room_sound(|assets| &assets.room_leave),
        NotificationCue::RoomMessage => play_room_sound(|assets| &assets.room_message),
    }
}

fn play_room_sound(path_fn: impl FnOnce(&SoundAssets) -> &Path) {
    let Some(assets) = sound_assets() else {
        play_terminal_bell();
        return;
    };
    if !spawn_player(path_fn(assets)) {
        play_terminal_bell();
    }
}

fn sound_assets() -> Option<&'static SoundAssets> {
    SOUND_ASSETS
        .get_or_init(|| materialize_sound_assets().ok())
        .as_ref()
}

fn materialize_sound_assets() -> io::Result<SoundAssets> {
    let dir = std::env::temp_dir().join("agentbook-tui-sounds-v1");
    fs::create_dir_all(&dir)?;

    let room_join = write_asset_if_missing(
        &dir,
        "aim-door-open.wav",
        include_bytes!("../assets/sounds/aim-door-open.wav"),
    )?;
    let room_leave = write_asset_if_missing(
        &dir,
        "aim-door-close.wav",
        include_bytes!("../assets/sounds/aim-door-close.wav"),
    )?;
    let room_message = write_asset_if_missing(
        &dir,
        "aim-ping.wav",
        include_bytes!("../assets/sounds/aim-ping.wav"),
    )?;

    Ok(SoundAssets {
        room_join,
        room_leave,
        room_message,
    })
}

fn write_asset_if_missing(dir: &Path, file_name: &str, bytes: &[u8]) -> io::Result<PathBuf> {
    let path = dir.join(file_name);
    if !path.exists() {
        fs::write(&path, bytes)?;
    }
    Ok(path)
}

fn spawn_player(path: &Path) -> bool {
    #[cfg(target_os = "macos")]
    {
        let candidates = [
            ("/usr/bin/afplay", Vec::<&str>::new()),
            ("afplay", Vec::<&str>::new()),
        ];
        return spawn_first_supported(path, &candidates);
    }

    #[cfg(target_os = "linux")]
    {
        let candidates = [
            ("paplay", Vec::<&str>::new()),
            ("pw-play", Vec::<&str>::new()),
            ("aplay", vec!["-q"]),
            ("play", vec!["-q"]),
        ];
        return spawn_first_supported(path, &candidates);
    }

    #[cfg(target_os = "windows")]
    {
        let command = format!(
            "(New-Object Media.SoundPlayer '{}').PlaySync()",
            path.display()
        );
        return Command::new("powershell")
            .args(["-NoProfile", "-NonInteractive", "-Command", &command])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map(reap_child)
            .is_ok();
    }

    #[allow(unreachable_code)]
    false
}

fn spawn_first_supported(path: &Path, candidates: &[(&str, Vec<&str>)]) -> bool {
    for (program, args) in candidates {
        let mut command = Command::new(program);
        command
            .args(args)
            .arg(path)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        if let Ok(child) = command.spawn() {
            reap_child(child);
            return true;
        }
    }
    false
}

fn reap_child(mut child: std::process::Child) {
    std::thread::spawn(move || {
        let _ = child.wait();
    });
}

fn play_terminal_bell() {
    let mut stderr = io::stderr();
    let _ = stderr.write_all(b"\x07");
    let _ = stderr.flush();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sound_assets_materialize() {
        let assets = sound_assets().expect("sound assets should materialize");
        assert!(assets.room_join.exists());
        assert!(assets.room_leave.exists());
        assert!(assets.room_message.exists());
    }
}
