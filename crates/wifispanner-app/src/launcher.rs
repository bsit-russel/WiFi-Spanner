// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Byte Size IT. See the LICENSE and NOTICE files.

//! Desktop integration for the AppImage: a launcher entry and icon in the
//! user's home folder, so the desktop shows the WiFi Spanner icon in the
//! taskbar and title bar and lists the app in the menu. An AppImage does not
//! install these itself, and desktops match a window to its icon through a
//! `.desktop` file named after the window's app id (`wifispanner`).

use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::store;

const ICON_NAME: &str = "wifispanner";
const LOGO_PNG: &[u8] = include_bytes!("../../../assets/logo.png");

pub fn desktop_file() -> PathBuf {
    store::data_home().join("applications").join("wifispanner.desktop")
}

pub fn icon_file() -> PathBuf {
    store::data_home().join("icons/hicolor/256x256/apps").join(format!("{ICON_NAME}.png"))
}

pub fn is_installed() -> bool {
    desktop_file().is_file()
}

/// Quote a path for a `.desktop` `Exec` line: inside double quotes, the
/// characters `"`, `` ` ``, `$` and `\` need a backslash (and `\` is doubled
/// once more by the format's own string escaping).
fn exec_quote(path: &Path) -> Result<String, String> {
    let p = path.to_str().ok_or("the AppImage path is not valid text")?;
    if p.chars().any(|c| c.is_control()) {
        return Err("the AppImage path contains control characters".into());
    }
    let mut out = String::from("\"");
    for c in p.chars() {
        match c {
            '\\' => out.push_str("\\\\\\\\"),
            '"' | '`' | '$' => {
                out.push_str("\\\\");
                out.push(c);
            }
            '%' => out.push_str("%%"),
            c => out.push(c),
        }
    }
    out.push('"');
    Ok(out)
}

fn render_desktop(exec: &Path) -> Result<String, String> {
    Ok(format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Name=WiFi Spanner\n\
         Comment=Your one-stop WiFi Scanner and Planner\n\
         Exec={} %U\n\
         Icon={ICON_NAME}\n\
         Terminal=false\n\
         Categories=Network;Utility;\n\
         StartupWMClass=wifispanner\n",
        exec_quote(exec)?
    ))
}

fn icon_png() -> Result<Vec<u8>, String> {
    let img = image::load_from_memory(LOGO_PNG).map_err(|e| e.to_string())?;
    let small = img.resize(256, 256, image::imageops::FilterType::Lanczos3);
    let mut out = Vec::new();
    small.write_to(&mut Cursor::new(&mut out), image::ImageFormat::Png).map_err(|e| e.to_string())?;
    Ok(out)
}

/// Write the launcher and icon for the AppImage at `exec`.
pub fn install(exec: &Path) -> Result<(), String> {
    let (desktop, icon) = (desktop_file(), icon_file());
    let text = render_desktop(exec)?;
    let png = icon_png()?;
    for (path, bytes) in [(&icon, png.as_slice()), (&desktop, text.as_bytes())] {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("could not create {}: {e}", dir.display()))?;
        }
        std::fs::write(path, bytes).map_err(|e| format!("could not write {}: {e}", path.display()))?;
    }
    refresh_desktop_caches();
    Ok(())
}

/// Remove what `install` wrote. Missing files are fine.
pub fn remove() -> Result<(), String> {
    for path in [desktop_file(), icon_file()] {
        match std::fs::remove_file(&path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(format!("could not remove {}: {e}", path.display())),
        }
    }
    refresh_desktop_caches();
    Ok(())
}

/// If a launcher exists but points at a different file (the AppImage was
/// moved or renamed), rewrite it. Returns true if it changed anything.
pub fn refresh_if_stale(exec: &Path) -> bool {
    let Ok(current) = std::fs::read_to_string(desktop_file()) else { return false };
    match render_desktop(exec) {
        Ok(wanted) if wanted != current => install(exec).is_ok(),
        _ => false,
    }
}

/// Ask the desktop to notice the new files. Best effort and unwaited: the
/// tools may not exist, and desktops also watch these folders themselves.
fn refresh_desktop_caches() {
    let apps = store::data_home().join("applications");
    for (tool, args) in [("update-desktop-database", vec![apps.into_os_string()]), ("kbuildsycoca6", vec![])] {
        let _ = Command::new(tool).args(args).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Point the data home at a fresh folder for the test.
    fn sandbox(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ws-launcher-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // Safety: only this module's tests touch XDG_DATA_HOME, and each runs
        // as a single test so they never race each other.
        unsafe { std::env::set_var("XDG_DATA_HOME", &dir) };
        dir
    }

    #[test]
    fn exec_paths_are_quoted_for_the_desktop_format() {
        assert_eq!(exec_quote(Path::new("/home/me/My Apps/WiFi.AppImage")).unwrap(), "\"/home/me/My Apps/WiFi.AppImage\"");
        assert_eq!(exec_quote(Path::new("/a/b\"c$d`e")).unwrap(), "\"/a/b\\\\\"c\\\\$d\\\\`e\"");
        assert_eq!(exec_quote(Path::new("/100%/app")).unwrap(), "\"/100%%/app\"");
        assert!(exec_quote(Path::new("/bad\npath")).is_err());
    }

    #[test]
    fn launcher_lifecycle_install_refresh_remove() {
        let dir = sandbox("life");
        let app = Path::new("/home/me/Apps/WiFiSpanner.AppImage");
        assert!(!is_installed());

        install(app).unwrap();
        assert!(is_installed());
        let text = std::fs::read_to_string(desktop_file()).unwrap();
        assert!(text.contains("Exec=\"/home/me/Apps/WiFiSpanner.AppImage\" %U"));
        assert!(text.contains("StartupWMClass=wifispanner") && text.contains("Icon=wifispanner"));
        let png = std::fs::read(icon_file()).unwrap();
        let img = image::load_from_memory(&png).unwrap();
        assert_eq!((img.width(), img.height()), (256, 256));

        assert!(!refresh_if_stale(app), "an up-to-date launcher is left alone");
        let moved = Path::new("/elsewhere/WiFiSpanner.AppImage");
        assert!(refresh_if_stale(moved), "a moved AppImage updates the launcher");
        assert!(std::fs::read_to_string(desktop_file()).unwrap().contains("/elsewhere/WiFiSpanner.AppImage"));

        remove().unwrap();
        assert!(!is_installed() && !icon_file().exists());
        remove().unwrap();
        assert!(!refresh_if_stale(app), "nothing to refresh once removed");
        std::fs::remove_dir_all(dir).ok();
    }
}
