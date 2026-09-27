//! Self-update for the Windows client: on launch, checks the latest GitHub release; if it's newer,
//! downloads the new dogtag.exe, swaps it in, and relaunches - no separate updater needed.
//!
//! Windows won't let us overwrite our own running exe (the file is locked), but renaming it is fine:
//! current -> `dogtag.exe.old`, new -> `dogtag.exe`, spawn the new one, exit. The `.old` file is
//! cleaned up by the NEW process on ITS OWN next startup, by which point the old process has released
//! the lock. Never fails hard: any error along the way just skips the update for this run, so a flaky
//! network or a GitHub outage never blocks playing.
use serde::Deserialize;
use std::path::{Path, PathBuf};

const REPO: &str = "klem-s/dogtag";

#[derive(Deserialize)]
struct Release {
    tag_name: String,
    assets: Vec<Asset>,
}

#[derive(Deserialize)]
struct Asset {
    name: String,
    browser_download_url: String,
}

fn suffixed(exe: &Path, suffix: &str) -> PathBuf {
    let mut s = exe.as_os_str().to_os_string();
    s.push(suffix);
    PathBuf::from(s)
}

/// Deletes a leftover `.old` file from a previous update. Best-effort: called on every startup, so a
/// failure here (e.g. somehow still locked) just means it's cleaned up next time instead.
pub fn cleanup(exe: &Path) {
    let _ = std::fs::remove_file(suffixed(exe, ".old"));
}

/// Checks for a newer release; if found, downloads it, swaps it in, and relaunches with the same args.
/// Returns true if it relaunched - the caller must return immediately without doing anything else (the
/// new process is already running independently).
pub async fn check_and_relaunch(args: &[String]) -> bool {
    match try_update(args).await {
        Ok(relaunched) => relaunched,
        Err(e) => {
            eprintln!("[update] pas de mise à jour ({e:#})");
            false
        }
    }
}

async fn try_update(args: &[String]) -> anyhow::Result<bool> {
    let client = reqwest::Client::builder()
        .user_agent("dogtag-updater")
        .timeout(std::time::Duration::from_secs(10))
        .build()?;
    let rel: Release = client
        .get(format!("https://api.github.com/repos/{REPO}/releases/latest"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let latest = rel.tag_name.trim_start_matches('v');
    if latest == env!("CARGO_PKG_VERSION") {
        return Ok(false);
    }
    let Some(asset) = rel.assets.iter().find(|a| a.name == "dogtag.exe") else {
        return Ok(false); // release with no exe attached - nothing to fetch, skip quietly
    };
    eprintln!("[update] {} -> {latest}, téléchargement...", env!("CARGO_PKG_VERSION"));
    let bytes = client.get(&asset.browser_download_url).send().await?.error_for_status()?.bytes().await?;

    let exe = std::env::current_exe()?;
    let new = suffixed(&exe, ".new");
    std::fs::write(&new, &bytes)?;
    std::fs::rename(&exe, suffixed(&exe, ".old"))?;
    std::fs::rename(&new, &exe)?;

    eprintln!("[update] {latest} installé, relance...");
    std::process::Command::new(&exe).args(args).spawn()?;
    Ok(true)
}
