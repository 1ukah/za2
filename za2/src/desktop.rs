//! The desktop entry and icon. Wayland compositors take a window's taskbar icon from the desktop entry named after
//! the application id (`za2`), not from the window, so za2 places both in the user's folders once (and again when
//! the binary moves), unless install.sh has installed them system-wide.

use std::path::{Path, PathBuf};

const ICON: &[u8] = include_bytes!("../ui/brand/icon-256.png");

fn share_dir() -> PathBuf {
    std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
        .unwrap_or_else(|| PathBuf::from("."))
}

fn entry(exe: &Path) -> String {
    format!(
        "[Desktop Entry]\nType=Application\nName=za2\nGenericName=zodiak for Aion 2\nComment=DPS meter and map for Aion 2\nExec=\"{}\"\nIcon=za2\nTerminal=false\nCategories=Game;Utility;\nStartupWMClass=za2\n",
        exe.display()
    )
}

/// Writes `data` to `path` unless it already holds exactly that. Returns whether it wrote.
fn write_if_changed(path: &Path, data: &[u8]) -> bool {
    if std::fs::read(path).is_ok_and(|old| old == data) {
        return false;
    }
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    std::fs::write(path, data).is_ok()
}

pub fn install() {
    let Ok(exe) = std::env::current_exe() else { return };
    // install.sh puts the entry under the same prefix as the binary (<prefix>/share/applications); a per-user copy is not needed.
    if exe.parent().is_some_and(|bin| bin.join("../share/applications/za2.desktop").is_file()) {
        return;
    }
    let share = share_dir();
    let icon = write_if_changed(&share.join("icons/hicolor/256x256/apps/za2.png"), ICON);
    let desktop = write_if_changed(&share.join("applications/za2.desktop"), entry(&exe).as_bytes());
    if icon || desktop {
        // Plasma notices new entries by itself after a while; this makes it immediate where the tool exists.
        let _ = std::process::Command::new("kbuildsycoca6").stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).spawn();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entry_names_the_window_class_and_icon() {
        let text = entry(Path::new("/opt/za2/za2"));
        assert!(text.contains("Exec=\"/opt/za2/za2\"") && text.contains("Icon=za2") && text.contains("StartupWMClass=za2"));
        assert_eq!(&ICON[1..4], b"PNG");
    }
}
