// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Byte Size IT. See the LICENSE and NOTICE files.

//! Checking GitHub for a newer release.
//!
//! The releases are read from GitHub's API. For a private repository that is
//! refused to anonymous requests, so the check falls back to the `gh` CLI
//! when it is installed and signed in.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const REPO: &str = "bsit-russel/WiFi-Spanner";
/// Where release pages and downloads must be hosted; anything else in the reply is not trusted.
const GITHUB: &str = "https://github.com/";
/// The largest AppImage accepted (the real one is about 30 MB).
const MAX_ASSET_BYTES: u64 = 200 << 20;
/// Redirects followed: the API sends a renamed or moved repository on once, a download goes on to
/// GitHub's storage.
const MAX_REDIRECTS: u32 = 4;

/// An HTTP client that speaks only HTTPS (redirects included) and follows a few redirects at most.
fn agent(timeout: Duration) -> ureq::Agent {
    // The tests serve the download from a local plain-HTTP port, so only they may use HTTP.
    agent_with(timeout, !cfg!(test))
}

fn agent_with(timeout: Duration, https_only: bool) -> ureq::Agent {
    ureq::Agent::config_builder().timeout_global(Some(timeout)).https_only(https_only).max_redirects(MAX_REDIRECTS).build().into()
}

/// Is `url` a place releases are downloaded from? (The tests' local server is allowed too.)
fn trusted_download(url: &str) -> bool {
    cfg!(test) || url.starts_with(GITHUB)
}

/// A release page on GitHub, or the repository's latest release when the reply names anything else.
fn release_page(url: String) -> String {
    if url.starts_with(GITHUB) && url.chars().all(|c| c.is_ascii_graphic()) { url } else { format!("{GITHUB}{REPO}/releases/latest") }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateInfo {
    /// Release tag, e.g. `v0.3.1`.
    pub tag: String,
    /// The release's page, where the AppImage can be downloaded.
    pub url: String,
    /// The AppImage to install in place, when the release has one with a
    /// checksum we can verify.
    pub asset: Option<Asset>,
    /// What changed, one entry per line of the release notes.
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Asset {
    pub url: String,
    pub size: u64,
    /// Lower-case hex SHA-256 published by GitHub for the file.
    pub sha256: String,
}

#[derive(Deserialize)]
struct Release {
    tag_name: String,
    html_url: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    assets: Vec<AssetJson>,
}

#[derive(Deserialize)]
struct AssetJson {
    name: String,
    size: u64,
    digest: Option<String>,
    browser_download_url: String,
}

/// The release's `.AppImage` with a usable `sha256:` digest, if any. Without
/// a checksum there is nothing to verify the download against, so the app
/// falls back to linking the release page.
fn pick_asset(assets: Vec<AssetJson>) -> Option<Asset> {
    assets.into_iter().find_map(|a| {
        let hex = a.digest.as_deref()?.strip_prefix("sha256:")?.to_ascii_lowercase();
        let ok = a.name.ends_with(".AppImage")
            && a.size > 0
            && a.size <= MAX_ASSET_BYTES
            && a.browser_download_url.starts_with(GITHUB)
            && a.browser_download_url.chars().all(|c| c.is_ascii_graphic())
            && hex.len() == 64
            && hex.chars().all(|c| c.is_ascii_hexdigit());
        ok.then(|| Asset { url: a.browser_download_url, size: a.size, sha256: hex })
    })
}

/// Look for a release newer than `current` (a version like `0.3.0`).
/// `Ok(None)` means this is the newest.
pub fn check(current: &str) -> Result<Option<UpdateInfo>, String> {
    let json = fetch_public().or_else(|public_err| {
        fetch_with_gh().map_err(|gh_err| {
            format!("GitHub did not share the release list ({public_err}), and the GitHub CLI could not ({gh_err})")
        })
    })?;
    newest_after(&json, current)
}

fn fetch_public() -> Result<String, String> {
    let mut response = agent(Duration::from_secs(8))
        .get(format!("https://api.github.com/repos/{REPO}/releases?per_page=30"))
        .header("User-Agent", concat!("wifispanner/", env!("CARGO_PKG_VERSION")))
        .header("Accept", "application/vnd.github+json")
        .call()
        .map_err(|e| e.to_string())?;
    response.body_mut().read_to_string().map_err(|e| e.to_string())
}

fn fetch_with_gh() -> Result<String, String> {
    let out = Command::new("gh")
        .args(["api", &format!("repos/{REPO}/releases?per_page=30")])
        .output()
        .map_err(|e| format!("gh not available: {e}"))?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// `0.3.1`, `v0.3.1` or `v0.3.1-rc1` as comparable numbers.
fn parse_version(s: &str) -> Option<(u64, u64, u64)> {
    let core = s.trim().trim_start_matches('v').split(['-', '+']).next()?;
    let mut parts = core.split('.');
    let mut next = || parts.next()?.parse::<u64>().ok();
    let v = (next()?, next()?, next()?);
    parts.next().is_none().then_some(v)
}

/// A release's notes as plain lines: Markdown bullets and headings lose their marks, blank lines go.
fn notes_from(body: Option<&str>) -> Vec<String> {
    body.unwrap_or("")
        .lines()
        .map(|l| l.trim().trim_start_matches(['-', '*', '#', '+', ' ']).replace("**", "").replace('`', ""))
        .filter(|l| !l.is_empty())
        .collect()
}

/// The highest published release, if it is newer than `current`. Drafts are
/// ignored; pre-releases count, since every release so far is one.
fn newest_after(json: &str, current: &str) -> Result<Option<UpdateInfo>, String> {
    let current = parse_version(current).ok_or_else(|| format!("unrecognised app version {current:?}"))?;
    let releases: Vec<Release> = serde_json::from_str(json).map_err(|e| format!("unexpected reply from GitHub: {e}"))?;
    Ok(releases
        .into_iter()
        .filter(|r| !r.draft)
        .filter_map(|r| Some((parse_version(&r.tag_name)?, r)))
        .max_by_key(|(v, _)| *v)
        .filter(|(v, _)| *v > current)
        .map(|(_, r)| UpdateInfo { notes: notes_from(r.body.as_deref()), asset: pick_asset(r.assets), tag: r.tag_name, url: release_page(r.html_url) }))
}

/// Which update notices to stay quiet about: a version the user skipped, and
/// a "remind me tomorrow" snooze. Saved beside the settings.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Prefs {
    /// The newest version the user chose to dismiss; a newer one is announced again.
    pub skipped: Option<String>,
    /// Unix time before which the automatic notice stays hidden...
    pub snooze_until: u64,
    /// ...for this version only: a newer release is announced at once.
    pub snoozed: Option<String>,
}

const SNOOZE_SECS: u64 = 24 * 60 * 60;

impl Prefs {
    fn path() -> PathBuf {
        crate::store::config_dir().join("update.json")
    }

    pub fn load() -> Prefs {
        std::fs::read_to_string(Self::path()).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
    }

    /// Best effort, like the settings.
    pub fn save(&self) {
        let path = Self::path();
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(text) = serde_json::to_string_pretty(self) {
            let _ = std::fs::write(path, text);
        }
    }

    /// Whether the automatic check should announce `tag` at time `now`.
    pub fn should_announce(&self, tag: &str, now: u64) -> bool {
        self.skipped.as_deref() != Some(tag) && (self.snoozed.as_deref() != Some(tag) || now >= self.snooze_until)
    }

    pub fn snooze(&mut self, tag: &str, now: u64) {
        self.snooze_until = now + SNOOZE_SECS;
        self.snoozed = Some(tag.to_string());
    }

    pub fn skip(&mut self, tag: &str) {
        self.skipped = Some(tag.to_string());
    }
}

pub fn unix_now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// The AppImage file this process runs from. `None` for any other build, in
/// which case an update can only be offered as a link.
pub fn running_appimage() -> Option<PathBuf> {
    std::env::var_os("APPIMAGE").map(PathBuf::from).filter(|p| p.is_absolute() && p.is_file())
}

/// Download `asset`, check it against its published checksum and replace
/// `target` with it. The old file stays untouched unless every check passes.
/// `progress` gets the bytes downloaded so far; setting `cancel` aborts.
pub fn install(asset: &Asset, target: &Path, cancel: &AtomicBool, progress: impl FnMut(u64)) -> Result<(), String> {
    let name = target.file_name().map_or("app".into(), |n| n.to_string_lossy().into_owned());
    if asset.size > MAX_ASSET_BYTES || !trusted_download(&asset.url) {
        return Err("the update is not from where it should be, so it was not downloaded".into());
    }
    // The download goes to a file beside the app with a name nobody could have guessed, created
    // only if it is new, so nothing already there (a link, say) can be written through.
    let (tmp, file) = create_temp(target, &name)?;
    let result = download(&asset.url, file, &tmp, asset.size, cancel, progress)
        .and_then(|sha| replace(&tmp, target, &sha, &asset.sha256));
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

/// A new file beside `target` with a random name, readable and writable by this user only.
fn create_temp(target: &Path, name: &str) -> Result<(PathBuf, std::fs::File), String> {
    use std::hash::{BuildHasher, Hasher};
    for _ in 0..8 {
        // RandomState is seeded from the system's random source for every instance.
        let token = std::collections::hash_map::RandomState::new().build_hasher().finish() ^ (unix_nanos() as u64);
        let path = target.with_file_name(format!(".{name}.update-{token:016x}"));
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        match options.open(&path) {
            Ok(file) => return Ok((path, file)),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(format!("could not write beside {}: {e}", target.display())),
        }
    }
    Err("could not make a file for the download".into())
}

fn unix_nanos() -> u128 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos())
}

/// Stream `url` into `file` (at `dest`), returning the SHA-256 of what was written. The
/// body must be exactly `expected_size` bytes.
fn download(url: &str, mut file: std::fs::File, dest: &Path, expected_size: u64, cancel: &AtomicBool, mut progress: impl FnMut(u64)) -> Result<String, String> {
    let mut response = agent(Duration::from_secs(900))
        .get(url)
        .header("User-Agent", concat!("wifispanner/", env!("CARGO_PKG_VERSION")))
        .call()
        .map_err(|e| format!("download failed: {e}"))?;
    let mut reader = response.body_mut().as_reader();
    let (mut hasher, mut buf, mut done) = (Sha256::new(), vec![0u8; 64 * 1024], 0u64);
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err("cancelled".into());
        }
        let n = reader.read(&mut buf).map_err(|e| format!("download interrupted: {e}"))?;
        if n == 0 {
            break;
        }
        done += n as u64;
        if done > expected_size {
            return Err("the download is larger than the release says".into());
        }
        hasher.update(&buf[..n]);
        file.write_all(&buf[..n]).map_err(|e| format!("could not write {}: {e}", dest.display()))?;
        progress(done);
    }
    file.flush().map_err(|e| e.to_string())?;
    if done != expected_size {
        return Err(format!("the download is {done} bytes but the release says {expected_size}"));
    }
    Ok(hasher.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

/// Verify `tmp` and move it over `target` in one atomic step, keeping the
/// target's permissions (or making it executable).
fn replace(tmp: &Path, target: &Path, actual_sha: &str, expected_sha: &str) -> Result<(), String> {
    if !actual_sha.eq_ignore_ascii_case(expected_sha) {
        return Err("the download does not match its published checksum, so it was discarded".into());
    }
    let mut magic = [0u8; 4];
    std::fs::File::open(tmp).and_then(|mut f| f.read_exact(&mut magic)).map_err(|e| e.to_string())?;
    if &magic != b"\x7fELF" {
        return Err("the download is not an executable program".into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(target).map_or(0o755, |m| m.permissions().mode() | 0o111);
        std::fs::set_permissions(tmp, std::fs::Permissions::from_mode(mode)).map_err(|e| e.to_string())?;
    }
    std::fs::rename(tmp, target).map_err(|e| format!("could not replace {}: {e}", target.display()))
}

/// Start `target` as a fresh process with this one's arguments. The AppImage
/// variables are cleared so the new copy sets up its own mount.
pub fn restart(target: &Path) -> Result<(), String> {
    Command::new(target)
        .args(std::env::args_os().skip(1))
        .env_remove("APPIMAGE")
        .env_remove("APPDIR")
        .env_remove("ARGV0")
        .env_remove("OWD")
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("could not restart: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const JSON: &str = r#"[
        {"tag_name":"v0.2.0","html_url":"https://github.com/example/0.2.0","draft":false,"prerelease":true},
        {"tag_name":"v0.10.0","html_url":"https://github.com/example/0.10.0","draft":false,"prerelease":true},
        {"tag_name":"v9.9.9","html_url":"https://github.com/example/draft","draft":true,"prerelease":false},
        {"tag_name":"nightly","html_url":"https://github.com/example/nightly","draft":false,"prerelease":true},
        {"tag_name":"v0.1.1","html_url":"https://github.com/example/0.1.1","draft":false,"prerelease":true}
    ]"#;

    #[test]
    fn release_notes_become_plain_lines() {
        let got = notes_from(Some("## New\r\n- **Carbon** toast\n\n* `data` table"));
        assert_eq!(got, vec!["New", "Carbon toast", "data table"]);
        assert!(notes_from(None).is_empty());
    }

    #[test]
    fn picks_the_highest_release_comparing_numbers_not_text() {
        let got = newest_after(JSON, "0.3.0").unwrap().unwrap();
        assert_eq!(got, UpdateInfo { tag: "v0.10.0".into(), url: "https://github.com/example/0.10.0".into(), asset: None, notes: vec![] });
    }

    #[test]
    fn nothing_newer_means_up_to_date() {
        assert_eq!(newest_after(JSON, "0.10.0"), Ok(None));
        assert_eq!(newest_after(JSON, "1.0.0"), Ok(None));
    }

    #[test]
    fn drafts_and_odd_tags_are_ignored() {
        // v9.9.9 is a draft and "nightly" is not a version.
        assert!(newest_after(JSON, "0.0.1").unwrap().is_some_and(|u| u.tag == "v0.10.0"));
    }

    #[test]
    fn only_github_downloads_of_a_sensible_size_are_accepted() {
        let asset = |url: &str, size: u64| AssetJson { name: "WiFiSpanner-x86_64.AppImage".into(), size, digest: Some(format!("sha256:{}", "a".repeat(64))), browser_download_url: url.into() };
        let good = "https://github.com/bsit-russel/WiFi-Spanner/releases/download/v1.0.0/WiFiSpanner-x86_64.AppImage";
        assert!(pick_asset(vec![asset(good, 30 << 20)]).is_some());
        assert!(pick_asset(vec![asset("https://evil.example/x.AppImage", 30 << 20)]).is_none());
        assert!(pick_asset(vec![asset("https://github.com.evil.example/x.AppImage", 30 << 20)]).is_none());
        assert!(pick_asset(vec![asset("http://github.com/x.AppImage", 30 << 20)]).is_none());
        assert!(pick_asset(vec![asset(good, MAX_ASSET_BYTES + 1)]).is_none());
        assert!(pick_asset(vec![asset(&format!("{good}\nX"), 30 << 20)]).is_none());
    }

    #[test]
    fn a_release_page_off_github_is_replaced_by_the_repository_page() {
        assert_eq!(release_page("https://github.com/bsit-russel/WiFi-Spanner/releases/tag/v1.0.0".into()), "https://github.com/bsit-russel/WiFi-Spanner/releases/tag/v1.0.0");
        let fallback = format!("https://github.com/{REPO}/releases/latest");
        assert_eq!(release_page("https://evil.example/".into()), fallback);
        assert_eq!(release_page("javascript:alert(1)".into()), fallback);
        assert_eq!(release_page("https://github.com/x y".into()), fallback);
    }

    #[test]
    fn the_download_file_is_new_private_and_unpredictably_named() {
        let dir = std::env::temp_dir().join(format!("ws-update-test-{}", unix_nanos()));
        std::fs::create_dir_all(&dir).unwrap();
        let target = dir.join("app.AppImage");
        let (a, _fa) = create_temp(&target, "app.AppImage").unwrap();
        let (b, _fb) = create_temp(&target, "app.AppImage").unwrap();
        assert_ne!(a, b);
        assert!(a.file_name().unwrap().to_string_lossy().starts_with(".app.AppImage.update-"));
        assert!(!a.to_string_lossy().contains(&std::process::id().to_string()) || a != target.with_file_name(format!(".app.AppImage.update-{}", std::process::id())));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&a).unwrap().permissions().mode() & 0o777, 0o600);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_real_client_refuses_plain_http() {
        let err = agent_with(Duration::from_secs(2), true).get("http://127.0.0.1:9/x").call().unwrap_err().to_string();
        assert!(err.to_lowercase().contains("http"), "{err}");
    }

    #[test]
    fn version_parsing() {
        assert_eq!(parse_version("v1.2.3"), Some((1, 2, 3)));
        assert_eq!(parse_version("1.2.3-rc1"), Some((1, 2, 3)));
        assert_eq!(parse_version("1.2"), None);
        assert_eq!(parse_version("1.2.3.4"), None);
        assert_eq!(parse_version("nightly"), None);
    }

    #[test]
    fn bad_replies_are_errors() {
        assert!(newest_after("{\"message\":\"Not Found\"}", "0.1.0").is_err());
        assert!(newest_after("[]", "bogus").is_err());
    }

    /// Talks to GitHub: run with `cargo test -- --ignored live_check`.
    #[test]
    #[ignore]
    fn live_check() {
        eprintln!("public: {:?}", fetch_public().map(|b| b.len()));
        eprintln!("gh: {:?}", fetch_with_gh().map(|b| b.len()));
        // The production client (HTTPS only, few redirects) against the real API and the old repository name, which GitHub redirects.
        for repo in [REPO, "rubble-za/WiFi-Spanner"] {
            let r = agent_with(Duration::from_secs(8), true).get(format!("https://api.github.com/repos/{repo}/releases?per_page=1")).header("User-Agent", "wifispanner-test").call();
            eprintln!("https-only {repo}: {:?}", r.map(|r| r.status()).map_err(|e| e.to_string()));
        }
        let old = check("0.1.0").unwrap().expect("an older version should see a newer release");
        eprintln!("from 0.1.0 the newest is {old:?}");
        eprintln!("from 99.0.0: {:?}", check("99.0.0"));
    }

    fn asset_json(name: &str, digest: Option<&str>, url: &str) -> AssetJson {
        AssetJson { name: name.into(), size: 10, digest: digest.map(Into::into), browser_download_url: url.into() }
    }

    #[test]
    fn only_an_appimage_with_a_checksum_over_https_is_installable() {
        let hex = "ab".repeat(32);
        let good = format!("sha256:{}", hex.to_uppercase());
        let got = pick_asset(vec![
            asset_json("notes.txt", Some(&good), "https://github.com/x/notes.txt"),
            asset_json("a.AppImage", None, "https://github.com/x/nodigest"),
            asset_json("b.AppImage", Some("sha1:abc"), "https://github.com/x/sha1"),
            asset_json("c.AppImage", Some(&good), "http://x.test/plain"),
            asset_json("d.AppImage", Some(&good), "https://github.com/x/d.AppImage"),
        ])
        .unwrap();
        assert_eq!((got.url.as_str(), got.sha256.as_str(), got.size), ("https://github.com/x/d.AppImage", hex.as_str(), 10));
        assert!(pick_asset(vec![asset_json("e.AppImage", Some("sha256:zz"), "https://github.com/x/e")]).is_none());
    }

    #[test]
    fn skip_and_snooze_rules() {
        let mut p = Prefs::default();
        assert!(p.should_announce("v1.0.0", 1000));
        p.snooze("v1.0.0", 1000);
        assert!(!p.should_announce("v1.0.0", 1000 + SNOOZE_SECS - 1), "still snoozed");
        assert!(p.should_announce("v1.0.0", 1000 + SNOOZE_SECS), "a day later it speaks up again");
        assert!(p.should_announce("v1.0.1", 1000), "a newer version ignores the snooze");
        p.skip("v1.0.0");
        assert!(!p.should_announce("v1.0.0", u64::MAX), "skipped for good");
        assert!(p.should_announce("v1.0.1", u64::MAX), "but a newer version is announced");
        let back: Prefs = serde_json::from_str("{}").unwrap();
        assert_eq!(back, Prefs::default(), "old or empty files load");
    }

    /// Serve `body` once on a local port and return its URL.
    fn serve_once(body: Vec<u8>) -> String {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/app.AppImage", listener.local_addr().unwrap());
        std::thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            let mut req = [0u8; 2048];
            let _ = s.read(&mut req);
            let _ = write!(s, "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
            let _ = s.write_all(&body);
        });
        url
    }

    fn fake_appimage(extra: usize) -> Vec<u8> {
        let mut v = b"\x7fELF".to_vec();
        v.extend(std::iter::repeat_n(7u8, extra));
        v
    }

    fn sha_hex(bytes: &[u8]) -> String {
        Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ws-update-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn a_verified_download_replaces_the_app_and_keeps_it_executable() {
        use std::os::unix::fs::PermissionsExt;
        let dir = temp_dir("ok");
        let target = dir.join("WiFiSpanner.AppImage");
        std::fs::write(&target, b"old version").unwrap();
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o755)).unwrap();
        let body = fake_appimage(5000);
        let asset = Asset { url: serve_once(body.clone()), size: body.len() as u64, sha256: sha_hex(&body) };
        let mut last = 0;
        install(&asset, &target, &AtomicBool::new(false), |n| last = n).unwrap();
        assert_eq!(last, body.len() as u64);
        assert_eq!(std::fs::read(&target).unwrap(), body);
        assert_eq!(std::fs::metadata(&target).unwrap().permissions().mode() & 0o111, 0o111);
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1, "no temp file left behind");
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn a_bad_checksum_size_or_file_type_leaves_the_old_app_untouched() {
        let dir = temp_dir("bad");
        let target = dir.join("WiFiSpanner.AppImage");
        std::fs::write(&target, b"old version").unwrap();
        let body = fake_appimage(100);
        let cases = [
            ("wrong checksum", body.clone(), body.len() as u64, "00".repeat(32)),
            ("wrong size", body.clone(), body.len() as u64 + 1, sha_hex(&body)),
            ("too large", body.clone(), 10, sha_hex(&body)),
            ("not an executable", b"not an elf at all".to_vec(), 17, sha_hex(b"not an elf at all")),
        ];
        for (what, served, size, sha256) in cases {
            let asset = Asset { url: serve_once(served), size, sha256 };
            assert!(install(&asset, &target, &AtomicBool::new(false), |_| {}).is_err(), "{what}");
            assert_eq!(std::fs::read(&target).unwrap(), b"old version", "{what}: target changed");
            assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1, "{what}: temp file left behind");
        }
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn cancelling_stops_the_download_and_cleans_up() {
        let dir = temp_dir("cancel");
        let target = dir.join("WiFiSpanner.AppImage");
        std::fs::write(&target, b"old version").unwrap();
        let body = fake_appimage(100);
        let asset = Asset { url: serve_once(body.clone()), size: body.len() as u64, sha256: sha_hex(&body) };
        assert_eq!(install(&asset, &target, &AtomicBool::new(true), |_| {}), Err("cancelled".into()));
        assert_eq!(std::fs::read(&target).unwrap(), b"old version");
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);
        std::fs::remove_dir_all(dir).ok();
    }
}
