// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Byte Size IT. See the LICENSE and NOTICE files.

//! Where projects and their reports live on disk, and small file helpers.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::SystemTime;

pub const EXTENSION: &str = "wifispanner";

/// `$XDG_DATA_HOME`, falling back to `~/.local/share`.
pub fn data_home() -> PathBuf {
    std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
        .unwrap_or_else(|| PathBuf::from("."))
}

static PROJECTS_DIR: std::sync::RwLock<Option<PathBuf>> = std::sync::RwLock::new(None);

/// The projects folder the user chose, or `None` for the default.
pub fn set_projects_dir(dir: Option<PathBuf>) {
    *PROJECTS_DIR.write().unwrap_or_else(|e| e.into_inner()) = dir;
}

/// The chosen folder, else `<data home>/wifispanner/projects`.
pub fn projects_dir() -> PathBuf {
    PROJECTS_DIR.read().unwrap_or_else(|e| e.into_inner()).clone().unwrap_or_else(default_projects_dir)
}

pub fn default_projects_dir() -> PathBuf {
    data_home().join("wifispanner").join("projects")
}

/// Move everything in the projects folder `from` into `to` (projects and their report folders).
/// Nothing is overwritten: if `to` already has a file or folder of the same name, nothing is
/// moved and the clash is reported. Returns how many items moved.
pub fn move_projects(from: &Path, to: &Path) -> Result<usize, String> {
    let same = |a: &Path, b: &Path| matches!((a.canonicalize(), b.canonicalize()), (Ok(a), Ok(b)) if a == b);
    if same(from, to) {
        return Err("That is already the projects folder.".into());
    }
    if to.canonicalize().is_ok_and(|t| from.canonicalize().is_ok_and(|f| t.starts_with(f))) {
        return Err("Choose a folder outside the current projects folder.".into());
    }
    std::fs::create_dir_all(to).map_err(|e| format!("Could not create {}: {e}", to.display()))?;
    let items: Vec<PathBuf> = match std::fs::read_dir(from) {
        Ok(rd) => rd.filter_map(Result::ok).map(|e| e.path()).collect(),
        Err(_) => Vec::new(),
    };
    for item in &items {
        if let Some(name) = item.file_name()
            && to.join(name).exists()
        {
            return Err(format!("{} already has an item named {}; nothing was moved.", to.display(), name.to_string_lossy()));
        }
    }
    for item in &items {
        let dest = to.join(item.file_name().unwrap_or_default());
        move_item(item, &dest).map_err(|e| format!("Could not move {}: {e}", item.display()))?;
    }
    Ok(items.len())
}

/// Rename, or copy then delete when the two folders are on different disks.
fn move_item(from: &Path, to: &Path) -> std::io::Result<()> {
    if std::fs::rename(from, to).is_ok() {
        return Ok(());
    }
    if from.is_dir() {
        std::fs::create_dir_all(to)?;
        for e in std::fs::read_dir(from)? {
            let e = e?;
            move_item(&e.path(), &to.join(e.file_name()))?;
        }
        std::fs::remove_dir(from)
    } else {
        std::fs::copy(from, to)?;
        std::fs::remove_file(from)
    }
}

/// `$XDG_CONFIG_HOME/wifispanner`, falling back to `~/.config/wifispanner`.
pub fn config_dir() -> PathBuf {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("wifispanner")
}

/// A path in the projects folder for a new project, not clashing with an
/// existing file. The name is reduced to safe filename characters.
pub fn new_project_path(name: &str) -> std::io::Result<PathBuf> {
    let dir = projects_dir();
    std::fs::create_dir_all(&dir)?;
    let stem = sanitize(name);
    let mut path = dir.join(format!("{stem}.{EXTENSION}"));
    let mut n = 2;
    while path.exists() {
        path = dir.join(format!("{stem}-{n}.{EXTENSION}"));
        n += 1;
    }
    Ok(path)
}

fn sanitize(name: &str) -> String {
    let s: String = name
        .chars()
        .map(|c| if c.is_alphanumeric() || matches!(c, '-' | '_' | ' ') { c } else { '_' })
        .collect();
    let s = s.trim().trim_matches('.').to_string();
    if s.is_empty() { "survey".into() } else { s }
}

pub fn project_files() -> Vec<PathBuf> {
    let Ok(rd) = std::fs::read_dir(projects_dir()) else { return Vec::new() };
    rd.filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == EXTENSION))
        .collect()
}

/// Reports for a project live in a folder beside it: `<stem>-reports/`.
pub fn reports_dir(project: &Path) -> PathBuf {
    let stem = project.file_stem().map_or("project".into(), |s| s.to_string_lossy().into_owned());
    project.with_file_name(format!("{stem}-reports"))
}

pub struct ReportFile {
    pub path: PathBuf,
    pub modified: Option<SystemTime>,
}

/// Existing reports for a project, newest first.
pub fn reports_for(project: &Path) -> Vec<ReportFile> {
    let Ok(rd) = std::fs::read_dir(reports_dir(project)) else { return Vec::new() };
    let mut v: Vec<ReportFile> = rd
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "html"))
        .map(|path| {
            let modified = std::fs::metadata(&path).and_then(|m| m.modified()).ok();
            ReportFile { path, modified }
        })
        .collect();
    v.sort_by(|a, b| b.modified.cmp(&a.modified));
    v
}

/// Open a file or folder with the desktop's default application.
pub fn open_externally(path: &Path) -> Result<(), String> {
    Command::new("xdg-open")
        .arg(path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("could not run xdg-open: {e}"))
}

/// `file://` URI for a path, percent-encoding everything but the unreserved
/// characters and `/`.
fn file_uri(path: &Path) -> String {
    let mut out = String::from("file://");
    for b in path.to_string_lossy().bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Show `path` in the file manager with the file itself highlighted, through
/// the freedesktop `FileManager1` service (Dolphin, Nautilus and others). If
/// that is not available, open the folder instead.
pub fn reveal(path: &Path) -> Result<(), String> {
    let shown = Command::new("dbus-send")
        .args([
            "--session",
            "--print-reply",
            "--reply-timeout=3000",
            "--dest=org.freedesktop.FileManager1",
            "--type=method_call",
            "/org/freedesktop/FileManager1",
            "org.freedesktop.FileManager1.ShowItems",
        ])
        .arg(format!("array:string:{}", file_uri(path)))
        .arg("string:wifispanner")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success());
    if shown {
        return Ok(());
    }
    open_externally(path.parent().unwrap_or(path))
}

/// Local time via the system `date`, since the app has no time zone data.
pub fn format_unix(unix_secs: u64, format: &str) -> String {
    Command::new("date")
        .arg("-d")
        .arg(format!("@{unix_secs}"))
        .arg(format)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map_or_else(|| format!("unix {unix_secs}"), |o| String::from_utf8_lossy(&o.stdout).trim().to_string())
}

pub fn format_time(t: SystemTime) -> String {
    let secs = t.duration_since(SystemTime::UNIX_EPOCH).map_or(0, |d| d.as_secs());
    format_unix(secs, "+%Y-%m-%d %H:%M")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_become_safe_filenames() {
        assert_eq!(sanitize("Ground/Floor: 1"), "Ground_Floor_ 1");
        assert!(!sanitize("../..").contains(['/', '.']));
        assert_eq!(sanitize("  "), "survey");
        assert_eq!(sanitize("Plan-A_2"), "Plan-A_2");
    }

    #[test]
    fn reports_folder_sits_beside_the_project() {
        assert_eq!(reports_dir(Path::new("/a/b/Office.wifispanner")), PathBuf::from("/a/b/Office-reports"));
    }

    #[test]
    fn file_uris_are_percent_encoded() {
        assert_eq!(file_uri(Path::new("/home/me/My Reports/r-1.html")), "file:///home/me/My%20Reports/r-1.html");
        assert_eq!(file_uri(Path::new("/a/ü.html")), "file:///a/%C3%BC.html");
        assert_eq!(file_uri(Path::new("/a/b&c#d")), "file:///a/b%26c%23d");
    }
}

#[cfg(test)]
mod move_tests {
    use super::*;

    #[test]
    fn moves_projects_and_report_folders_and_refuses_clashes() {
        let base = std::env::temp_dir().join(format!("ws-move-{}", std::process::id()));
        let (a, b) = (base.join("a"), base.join("b"));
        std::fs::create_dir_all(a.join("x-reports")).unwrap();
        std::fs::write(a.join("x.wifispanner"), "1").unwrap();
        std::fs::write(a.join("x-reports/r.html"), "2").unwrap();
        assert_eq!(move_projects(&a, &b), Ok(2));
        assert!(b.join("x.wifispanner").is_file() && b.join("x-reports/r.html").is_file());
        assert!(!a.join("x.wifispanner").exists());
        std::fs::write(a.join("x.wifispanner"), "3").unwrap();
        assert!(move_projects(&a, &b).is_err());
        assert!(a.join("x.wifispanner").exists());
        assert!(move_projects(&b, &b.join("inner")).is_err());
        let _ = std::fs::remove_dir_all(base);
    }
}
