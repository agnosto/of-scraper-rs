//! Check-only update notices.
//!
//! Queries GitHub Releases for a newer version and reports it — that is
//! the entire behavior. There is deliberately no download, no extraction,
//! no execution and no binary replacement here: the check is one HTTPS
//! request plus a small cache file next to the config, so there's
//! nothing for antivirus software to be suspicious of.
//!
//! Results are cached for a day so repeated starts don't hammer the API
//! (unauthenticated GitHub allows 60 requests/hour). The check is
//! opt-in via `Updates.CheckForUpdates`; `of-scraper-rs check-update`
//! works regardless of the setting.

use std::{fs, path::PathBuf, sync::OnceLock};

use anyhow::Context;
use serde::{Deserialize, Serialize};

pub const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");

const LATEST_RELEASE_URL: &str = "https://api.github.com/repos/agnosto/of-scraper-rs/releases/latest";
const RELEASES_PAGE: &str = "https://github.com/agnosto/of-scraper-rs/releases/latest";
const CACHE_FILE: &str = "update-check.json";
const CACHE_TTL_SECS: i64 = 24 * 3600;

#[derive(Debug, Clone)]
pub struct UpdateInfo {
	/// Full tag as returned by GitHub, e.g. `v0.1.9`.
	pub latest: String,
	/// Release page on the web.
	pub url: String,
}

impl UpdateInfo {
	/// "Update available: v0.1.9 (installed: v0.1.8)"
	pub fn summary(&self) -> String {
		format!("Update available: {} (installed: v{})", self.latest, CURRENT_VERSION)
	}
}

/// What a fresh check found, for the `check-update` command's wording.
pub enum CheckOutcome {
	/// Newest release tag is not newer than the running version.
	UpToDate,
	Update(UpdateInfo),
}

// -- GitHub API shape ------------------------------------------------------------

#[derive(Deserialize)]
struct GithubRelease {
	tag_name: String,
	#[serde(default)]
	html_url: String,
}

// -- Version comparison ----------------------------------------------------------

/// Leading numeric components of a version-ish string: `v0.1.9` →
/// `[0, 1, 9]`, `1.2rc1` → `[1, 2]`. `None` when nothing numeric is found.
fn version_tuple(tag: &str) -> Option<Vec<u64>> {
	let trimmed = tag.trim().trim_start_matches(['v', 'V']);
	let mut out = Vec::new();
	for part in trimmed.split('.') {
		let digits: String = part.chars().take_while(|c| c.is_ascii_digit()).collect();
		out.push(digits.parse::<u64>().ok()?);
	}
	if out.is_empty() { None } else { Some(out) }
}

/// True when `latest_tag` is strictly newer than `current_version`.
/// Unparsable tags (error pages, prerelease-only strings) never count as
/// an update — no signal is better than a false one.
fn is_newer(latest_tag: &str, current_version: &str) -> bool {
	match (version_tuple(latest_tag), version_tuple(current_version)) {
		(Some(latest), Some(current)) => latest > current,
		_ => false,
	}
}

// -- Cache -----------------------------------------------------------------------

#[derive(Serialize, Deserialize)]
struct CacheFile {
	/// Unix seconds of the last successful check.
	checked_at: i64,
	latest_tag: String,
	html_url: String,
}

fn cache_path() -> PathBuf {
	// Same resolution as `state.db`: effective dir is wherever the other
	// config files live.
	crate::resolve_config_path(CACHE_FILE)
}

fn load_cache() -> Option<CacheFile> {
	let text = fs::read_to_string(cache_path()).ok()?;
	serde_json::from_str(&text).ok()
}

/// True when the cache entry is recent enough to answer a check without
/// hitting the network.
fn cache_is_fresh(checked_at: i64, now_unix_secs: i64) -> bool {
	now_unix_secs - checked_at < CACHE_TTL_SECS
}

/// Builds the notice out of a cache entry, `Some` only when the cached
/// tag is actually newer than the running version.
fn notice_from_cache(cache: &CacheFile, now_unix_secs: i64) -> Option<UpdateInfo> {
	if !cache_is_fresh(cache.checked_at, now_unix_secs) {
		return None;
	}
	if !is_newer(&cache.latest_tag, CURRENT_VERSION) {
		return None;
	}
	Some(UpdateInfo {
		latest: cache.latest_tag.clone(),
		url: if cache.html_url.is_empty() {
			RELEASES_PAGE.to_string()
		} else {
			cache.html_url.clone()
		},
	})
}

fn save_cache(info: &UpdateInfo) {
	let entry = CacheFile {
		checked_at: chrono::Utc::now().timestamp(),
		latest_tag: info.latest.clone(),
		html_url: info.url.clone(),
	};
	if let Ok(json) = serde_json::to_string_pretty(&entry) {
		let _ = fs::write(cache_path(), json);
	}
}

// -- Public API ------------------------------------------------------------------

fn updates_enabled() -> bool {
	crate::config::config().updates.check_for_updates
}

/// One HTTP round trip against the GitHub API, no cache involvement.
async fn fetch_latest() -> anyhow::Result<UpdateInfo> {
	let client = reqwest::Client::builder()
		.user_agent(format!("of-scraper-rs/{}", CURRENT_VERSION))
		.timeout(std::time::Duration::from_secs(5))
		.build()?;
	let release: GithubRelease = client
		.get(LATEST_RELEASE_URL)
		.send()
		.await
		.context("requesting the GitHub release info failed")?
		.error_for_status()
		.context("GitHub returned an error for the release lookup")?
		.json()
		.await
		.context("parsing the GitHub release info failed")?;

	Ok(UpdateInfo {
		latest: release.tag_name,
		url: if release.html_url.is_empty() {
			RELEASES_PAGE.to_string()
		} else {
			release.html_url
		},
	})
}

/// Fresh check, cache bypassed. Used by the explicit `check-update`
/// command; refreshes the 24h cache on success either way.
pub async fn check_now() -> anyhow::Result<CheckOutcome> {
	let info = fetch_latest().await?;
	save_cache(&info);
	if is_newer(&info.latest, CURRENT_VERSION) {
		Ok(CheckOutcome::Update(info))
	} else {
		Ok(CheckOutcome::UpToDate)
	}
}

/// Serialize concurrent automatic checks (TUI + anything else) so a
/// raced pair of callers share one network round trip: the second caller
/// re-reads the just-refreshed cache instead of re-fetching.
static CHECK_LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
fn check_lock() -> &'static tokio::sync::Mutex<()> {
	CHECK_LOCK.get_or_init(|| tokio::sync::Mutex::const_new(()))
}

/// Automatic, quiet check. Cache-first (`Some` notice only when the
/// cached release is newer); on a stale/missing cache performs one
/// fetch + cache write, silently giving up on any error. Respects the
/// `Updates.CheckForUpdates` opt-in.
pub async fn auto_check() -> Option<UpdateInfo> {
	if !updates_enabled() {
		return None;
	}
	if let Some(cache) = load_cache() {
		if let Some(info) = notice_from_cache(&cache, chrono::Utc::now().timestamp()) {
			return Some(info);
		}
		if cache_is_fresh(cache.checked_at, chrono::Utc::now().timestamp()) {
			return None;
		}
	}

	let _guard = check_lock().lock().await;
	// Another task may have finished the fetch while we waited for the lock.
	if let Some(cache) = load_cache() {
		if let Some(info) = notice_from_cache(&cache, chrono::Utc::now().timestamp()) {
			return Some(info);
		}
		if cache_is_fresh(cache.checked_at, chrono::Utc::now().timestamp()) {
			return None;
		}
	}

	match fetch_latest().await {
		Ok(info) => {
			save_cache(&info);
			is_newer(&info.latest, CURRENT_VERSION).then_some(info)
		}
		Err(err) => {
			log::debug!("Update check failed: {:#}", err);
			None
		}
	}
}

/// Instant, network-free notice for CLI startup — only trusts a fresh
/// cache; respects the opt-in.
pub fn cached_notice() -> Option<UpdateInfo> {
	if !updates_enabled() {
		return None;
	}
	load_cache().and_then(|c| notice_from_cache(&c, chrono::Utc::now().timestamp()))
}

/// Opens `url` with the platform's default handler. Best-effort, no new
/// dependencies: `xdg-open` / `open` / `cmd /C start ""`.
pub fn open_url(url: &str) {
	let (program, args): (&str, Vec<String>) = match std::env::consts::OS {
		"macos" => ("open", vec![url.to_string()]),
		"windows" => ("cmd", vec!["/C".into(), "start".into(), String::new(), url.to_string()]),
		_ => ("xdg-open", vec![url.to_string()]),
	};
	let _ = std::process::Command::new(program).args(args).spawn();
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn versions_compare_across_prefixes_and_padding() {
		assert!(is_newer("v0.1.9", "0.1.8"));
		assert!(is_newer("0.2.0", "v0.1.99"));
		assert!(is_newer("v1.0", "0.9.9"));
		// equal, older, and unparsable never report an update
		assert!(!is_newer("v0.1.8", "0.1.8"));
		assert!(!is_newer("v0.1.7", "0.1.8"));
		assert!(!is_newer("not-a-release", "0.1.8"));
		assert!(!is_newer("v0.1.8", "bogus"));
	}

	#[test]
	fn version_tuple_tracks_numeric_leading_parts() {
		assert_eq!(version_tuple("v0.1.8"), Some(vec![0, 1, 8]));
		assert_eq!(version_tuple("1.2rc1"), Some(vec![1, 2]));
		assert_eq!(version_tuple("nonsense"), None);
	}

	#[test]
	fn cache_freshness_window_is_24h() {
		let now = 1_700_000_000;
		assert!(cache_is_fresh(now, now));
		assert!(cache_is_fresh(now - CACHE_TTL_SECS + 1, now));
		assert!(!cache_is_fresh(now - CACHE_TTL_SECS, now));
		assert!(!cache_is_fresh(now - CACHE_TTL_SECS - 1, now));
	}

	#[test]
	fn notice_from_cache_requires_fresh_and_newer() {
		let now = 1_700_000_000;
		let mut cache = CacheFile {
			checked_at: now,
			latest_tag: "v99.0.0".to_string(),
			html_url: String::new(),
		};
		assert!(notice_from_cache(&cache, now).is_some());
		// falls back to the releases page when html_url is empty
		let info = notice_from_cache(&cache, now).unwrap();
		assert_eq!(info.url, RELEASES_PAGE);

		cache.checked_at = now - CACHE_TTL_SECS - 1;
		assert!(notice_from_cache(&cache, now).is_none());
	}
}
