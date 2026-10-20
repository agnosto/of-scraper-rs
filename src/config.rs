//! Typed view over the OFDL-style `config.conf` file, plus the filter /
//! formatting helpers that turn those settings into real behavior.
//!
//! The low-level line parser lives in `main.rs` (`CustomConfig`); this
//! module wraps it in a struct with typed fields and sane defaults, and
//! exposes app-wide globals so the scrape/download code can read settings
//! without threading a config object through every call site.

use std::{
	collections::{HashMap, HashSet},
	path::PathBuf,
	sync::{atomic::{AtomicBool, Ordering}, Mutex, OnceLock},
};

use chrono::{DateTime, NaiveDate, Utc};
use log::LevelFilter;
use of_client::{
	content::{Chat, ContentType, Post},
	media::{Feed, Media as _, MediaType},
};

use crate::{resolve_config_path, CustomConfig};

/// `Download.DownloadDateSelection` — whether `CustomDate` selects posts
/// before or after the cutoff date.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DateSelection {
	Before,
	After,
}

#[derive(Debug, Clone)]
pub struct MediaConfig {
	pub download_paid_posts: bool,
	pub download_posts: bool,
	pub download_archived: bool,
	pub download_stories: bool,
	pub download_highlights: bool,
	pub download_messages: bool,
	pub download_paid_messages: bool,
	pub download_images: bool,
	pub download_videos: bool,
	pub download_audios: bool,
}

impl Default for MediaConfig {
	fn default() -> Self {
		Self {
			download_paid_posts: true,
			download_posts: true,
			download_archived: true,
			download_stories: true,
			download_highlights: true,
			download_messages: true,
			download_paid_messages: true,
			download_images: true,
			download_videos: true,
			download_audios: true,
		}
	}
}

#[derive(Debug, Clone)]
pub struct DownloadConfig {
	pub media: MediaConfig,
	pub ignore_own_messages: bool,
	pub download_duplicated_media: bool,
	/// `SkipExistingMedia=true` (default): skip media whose id is already
	/// recorded in the download database (`state.db`) — it was downloaded on
	/// some previous run. Overridable per run with `scrape --force`.
	pub skip_existing_media: bool,
	pub skip_ads: bool,
	pub download_path: Option<PathBuf>,
	pub download_only_specific_dates: bool,
	pub download_date_selection: DateSelection,
	pub custom_date: Option<NaiveDate>,
	pub disable_text_sanitization: bool,
}

impl Default for DownloadConfig {
	fn default() -> Self {
		Self {
			media: MediaConfig::default(),
			ignore_own_messages: false,
			download_duplicated_media: false,
			skip_existing_media: true,
			skip_ads: false,
			download_path: None,
			download_only_specific_dates: false,
			download_date_selection: DateSelection::Before,
			custom_date: None,
			disable_text_sanitization: false,
		}
	}
}

/// `Metadata.SaveUserDb` — per-creator `user_data.db` capture (see
/// `src/metadata.rs`), DIGI-compatible so external importers (Stash etc.)
/// can read it.
#[derive(Debug, Clone, Default)]
pub struct MetadataConfig {
	pub save_user_db: bool,
}

#[derive(Debug, Clone, Default)]
pub struct FileConfig {
	pub paid_post_file_name_format: Option<String>,
	pub post_file_name_format: Option<String>,
	pub paid_message_file_name_format: Option<String>,
	pub message_file_name_format: Option<String>,
	pub rename_existing_files_when_custom_format_is_selected: bool,
}

#[derive(Debug, Clone, Default)]
pub struct FolderConfig {
	pub folder_per_paid_post: bool,
	pub folder_per_post: bool,
	pub folder_per_paid_message: bool,
	pub folder_per_message: bool,
}

#[derive(Debug, Clone, Default)]
pub struct SubscriptionsConfig {
	pub include_expired_subscriptions: bool,
	pub include_restricted_subscriptions: bool,
}

#[derive(Debug, Clone, Default)]
pub struct PerformanceConfig {
	/// Per-request HTTP timeout. `None` = reqwest default (no overall timeout).
	pub timeout: Option<std::time::Duration>,
	pub limit_download_rate: bool,
	pub download_limit_mb_per_sec: f64,
}

#[derive(Debug, Clone)]
pub struct AppConfig {
	pub download: DownloadConfig,
	pub file: FileConfig,
	pub folder: FolderConfig,
	pub subscriptions: SubscriptionsConfig,
	pub performance: PerformanceConfig,
	/// `External.FFmpegPath` — path to a custom ffmpeg binary.
	pub ffmpeg_path: Option<PathBuf>,
	pub metadata: MetadataConfig,
	pub logging_level: LevelFilter,
}

impl Default for AppConfig {
	fn default() -> Self {
		Self {
			download: DownloadConfig::default(),
			file: FileConfig::default(),
			folder: FolderConfig::default(),
			subscriptions: SubscriptionsConfig::default(),
			performance: PerformanceConfig::default(),
			ffmpeg_path: None,
			metadata: MetadataConfig::default(),
			logging_level: LevelFilter::Debug, // matches the pre-config behavior
		}
	}
}

impl AppConfig {
	pub fn load() -> Self {
		let mut cfg = AppConfig::default();

		let path = resolve_config_path("config.conf");
		if !path.exists() {
			return cfg;
		}
		let Ok(parsed) = CustomConfig::load(&path) else {
			return cfg;
		};

		cfg.download.media.download_paid_posts = bool_of(&parsed, "download.media", "downloadpaidposts", true);
		cfg.download.media.download_posts = bool_of(&parsed, "download.media", "downloadposts", true);
		cfg.download.media.download_archived = bool_of(&parsed, "download.media", "downloadarchived", true);
		cfg.download.media.download_stories = bool_of(&parsed, "download.media", "downloadstories", true);
		cfg.download.media.download_highlights = bool_of(&parsed, "download.media", "downloadhighlights", true);
		cfg.download.media.download_messages = bool_of(&parsed, "download.media", "downloadmessages", true);
		cfg.download.media.download_paid_messages = bool_of(&parsed, "download.media", "downloadpaidmessages", true);
		cfg.download.media.download_images = bool_of(&parsed, "download.media", "downloadimages", true);
		cfg.download.media.download_videos = bool_of(&parsed, "download.media", "downloadvideos", true);
		cfg.download.media.download_audios = bool_of(&parsed, "download.media", "downloadaudios", true);

		cfg.download.ignore_own_messages = bool_of(&parsed, "download", "ignoreownmessages", false);
		cfg.download.download_duplicated_media = bool_of(&parsed, "download", "downloadduplicatedmedia", false);
		cfg.download.skip_existing_media = bool_of(&parsed, "download", "skipexistingmedia", true);
		cfg.download.skip_ads = bool_of(&parsed, "download", "skipads", false);
		cfg.metadata.save_user_db = bool_of(&parsed, "metadata", "saveuserdb", false);
		cfg.download.download_only_specific_dates = bool_of(&parsed, "download", "downloadonlyspecificdates", false);
		cfg.download.disable_text_sanitization = bool_of(&parsed, "download", "disabletextsanitization", false);

		if let Some(p) = parsed.get("download", "downloadpath").and_then(trim_to_owned) {
			cfg.download.download_path = Some(PathBuf::from(p));
		}

		match parsed.get("download", "downloaddateselection").map(|s| s.to_lowercase()).as_deref() {
			Some("after") => cfg.download.download_date_selection = DateSelection::After,
			_ => cfg.download.download_date_selection = DateSelection::Before,
		}

		cfg.download.custom_date = parsed
			.get("download", "customdate")
			.and_then(trim_to_owned)
			.and_then(|s| NaiveDate::parse_from_str(&s, "%Y-%m-%d").ok());

		cfg.file.paid_post_file_name_format = str_of(&parsed, "file", "paidpostfilenameformat");
		cfg.file.post_file_name_format = str_of(&parsed, "file", "postfilenameformat");
		cfg.file.paid_message_file_name_format = str_of(&parsed, "file", "paidmessagefilenameformat");
		cfg.file.message_file_name_format = str_of(&parsed, "file", "messagefilenameformat");
		cfg.file.rename_existing_files_when_custom_format_is_selected = bool_of(&parsed, "file", "renameexistingfileswhencustomformatisselected", false);

		cfg.folder.folder_per_paid_post = bool_of(&parsed, "folder", "folderperpaidpost", false);
		cfg.folder.folder_per_post = bool_of(&parsed, "folder", "folderperpost", false);
		cfg.folder.folder_per_paid_message = bool_of(&parsed, "folder", "folderperpaidmessage", false);
		cfg.folder.folder_per_message = bool_of(&parsed, "folder", "foldermessage", false);

		cfg.subscriptions.include_expired_subscriptions = bool_of(&parsed, "subscriptions", "includeexpiredsubscriptions", false);
		cfg.subscriptions.include_restricted_subscriptions = bool_of(&parsed, "subscriptions", "includerestrictedsubscriptions", false);

		cfg.ffmpeg_path = str_of(&parsed, "external", "ffmpegpath").map(PathBuf::from);

		cfg.performance.timeout = parsed
			.get("performance", "timeout")
			.and_then(trim_to_owned)
			.and_then(|s| s.parse::<i64>().ok())
			.filter(|&secs| secs > 0)
			.map(|secs| std::time::Duration::from_secs(secs as u64));
		cfg.performance.limit_download_rate = bool_of(&parsed, "performance", "limitdownloadrate", false);
		cfg.performance.download_limit_mb_per_sec = parsed
			.get("performance", "downloadlimitinmbpersec")
			.and_then(trim_to_owned)
			.and_then(|s| s.parse::<f64>().ok())
			.unwrap_or(4.0);

		if let Some(level) = parsed.get("logging", "logginglevel").and_then(trim_to_owned) {
			cfg.logging_level = parse_level(&level);
		}

		cfg
	}
}

#[allow(clippy::ptr_arg)]
fn trim_to_owned(s: &String) -> Option<String> {
	let t = s.trim();
	if t.is_empty() {
		None
	} else {
		Some(t.to_string())
	}
}

fn bool_of(parsed: &CustomConfig, section: &str, key: &str, default: bool) -> bool {
	parsed.get(section, key).and_then(|s| s.parse::<bool>().ok()).unwrap_or(default)
}

fn str_of(parsed: &CustomConfig, section: &str, key: &str) -> Option<String> {
	parsed.get(section, key).and_then(trim_to_owned)
}

fn parse_level(s: &str) -> LevelFilter {
	match s.to_lowercase().as_str() {
		"off" => LevelFilter::Off,
		"error" => LevelFilter::Error,
		"warn" => LevelFilter::Warn,
		"info" => LevelFilter::Info,
		"debug" => LevelFilter::Debug,
		"trace" => LevelFilter::Trace,
		_ => LevelFilter::Error,
	}
}

// ---------------------------------------------------------------------------
// App-wide globals
// ---------------------------------------------------------------------------

static CONFIG: OnceLock<AppConfig> = OnceLock::new();
static MY_USER_ID: OnceLock<u64> = OnceLock::new();
static SEEN_MEDIA: OnceLock<Mutex<HashSet<u64>>> = OnceLock::new();
static FORCE_DOWNLOADS: AtomicBool = AtomicBool::new(false);
static METADATA_ONLY: AtomicBool = AtomicBool::new(false);

pub fn config() -> &'static AppConfig {
	CONFIG.get_or_init(AppConfig::load)
}

/// The authenticated user's numeric id, used by `IgnoreOwnMessages`.
pub fn set_my_user_id(id: u64) {
	let _ = MY_USER_ID.set(id);
}

pub fn my_user_id() -> Option<u64> {
	MY_USER_ID.get().copied()
}

/// Media ids already queued this process run, for `DownloadDuplicatedMedia`.
pub fn seen_media() -> &'static Mutex<HashSet<u64>> {
	SEEN_MEDIA.get_or_init(|| Mutex::new(HashSet::new()))
}

/// True when duplicate media (same media id twice in a run) should be skipped.
/// `DownloadDuplicatedMedia=false` (the default) means skip duplicates.
pub fn should_skip_duplicates() -> bool {
	!config().download.download_duplicated_media
}

/// `scrape --force` / `link --force`: bypass the db skip for this run.
pub fn set_force_downloads(force: bool) {
	FORCE_DOWNLOADS.store(force, Ordering::Relaxed);
}

pub fn force_downloads() -> bool {
	FORCE_DOWNLOADS.load(Ordering::Relaxed)
}

/// True when media should be skipped because it is already recorded in the
/// download database from a previous run.
pub fn skip_existing_media() -> bool {
	config().download.skip_existing_media && !force_downloads()
}

/// `scrape --metadata-only` / `link --metadata-only`: walk the API and fill
/// the metadata database, but never download a byte.
pub fn set_metadata_only(enabled: bool) {
	METADATA_ONLY.store(enabled, Ordering::Relaxed);
}

pub fn metadata_only() -> bool {
	METADATA_ONLY.load(Ordering::Relaxed)
}

// ---------------------------------------------------------------------------
// Content / media filter helpers
// ---------------------------------------------------------------------------

/// True if a whole content type is still allowed by the config toggles.
/// Acts as a hard filter on top of the TUI/CLI selection: false disables
/// the type even if the user checked it.
pub fn content_type_enabled(content_type: &str) -> bool {
	let media = &config().download.media;
	match content_type {
		"stories" => media.download_stories,
		"highlights" => media.download_highlights,
		_ => true,
	}
}

/// Whether the built-in "Archive" label is enabled (`DownloadArchived`).
pub fn archive_label_enabled() -> bool {
	config().download.media.download_archived
}

/// True if a label is the built-in archive pseudo-label.
pub fn is_archive_label(name: &str) -> bool {
	name.eq_ignore_ascii_case("archive") || name.eq_ignore_ascii_case("archived")
}

/// True when the post should be skipped entirely (`SkipAds`, paid/free toggles).
pub fn should_skip_post(post: &Post) -> bool {
	let cfg = &config().download;
	if cfg.skip_ads && text_is_ad(&post.text) {
		return true;
	}
	let paid = post.price.is_some();
	if paid && !cfg.media.download_paid_posts {
		return true;
	}
	if !paid && !cfg.media.download_posts {
		return true;
	}
	false
}

/// True when the message should be skipped entirely
/// (`SkipAds`, `IgnoreOwnMessages`, paid/free toggles).
pub fn should_skip_chat(chat: &Chat) -> bool {
	let cfg = &config().download;
	if cfg.skip_ads && text_is_ad(&chat.text) {
		return true;
	}
	let sender_id = chat.from_user.as_ref().map(|f| f.id);
	if cfg.ignore_own_messages && sender_id.is_some() && sender_id == my_user_id() {
		return true;
	}
	let paid = chat.price.is_some();
	if paid && !cfg.media.download_paid_messages {
		return true;
	}
	if !paid && !cfg.media.download_messages {
		return true;
	}
	false
}

fn text_is_ad(text: &str) -> bool {
	let lower = text.to_lowercase();
	lower.contains("#ad") || lower.contains("free trial") || lower.contains("onlyfans.com/free")
}

/// Result of applying the date filter to a single item.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DateFilter {
	Keep,
	Skip,
	/// Skip this item and stop paginating — nothing older qualifies.
	Break,
}

/// `DownloadOnlySpecificDates` + `CustomDate` + `DownloadDateSelection`,
/// applied to posts (which come back in descending date order).
pub fn post_date_filter(posted_at: DateTime<Utc>) -> DateFilter {
	let cfg = &config().download;
	if !cfg.download_only_specific_dates {
		return DateFilter::Keep;
	}
	let Some(cutoff) = cfg.custom_date else {
		return DateFilter::Keep;
	};
	let date = posted_at.date_naive();
	match cfg.download_date_selection {
		DateSelection::Before => {
			if date < cutoff {
				DateFilter::Keep
			} else {
				DateFilter::Skip
			}
		}
		DateSelection::After => {
			if date < cutoff {
				DateFilter::Break
			} else {
				DateFilter::Keep
			}
		}
	}
}

/// Whether media of a given type should be downloaded (`DownloadImages`,
/// `DownloadVideos`, `DownloadAudios`).
pub fn media_type_enabled(media_type: MediaType) -> bool {
	let media = &config().download.media;
	match media_type {
		MediaType::Photo => media.download_images,
		MediaType::Video | MediaType::Gif => media.download_videos,
		MediaType::Audio => media.download_audios,
	}
}

/// Human-readable name for a media type, for log messages.
pub fn media_type_name(media_type: MediaType) -> &'static str {
	match media_type {
		MediaType::Photo => "image",
		MediaType::Video => "video",
		MediaType::Gif => "gif",
		MediaType::Audio => "audio",
	}
}

// ---------------------------------------------------------------------------
// Folder structure + filename formatting
// ---------------------------------------------------------------------------

/// True when a per-content subfolder is requested for this content type.
pub fn folder_per_content(content_type: ContentType, is_paid: bool) -> bool {
	let folder = &config().folder;
	match (content_type, is_paid) {
		(ContentType::Posts, true) => folder.folder_per_paid_post,
		(ContentType::Posts, false) => folder.folder_per_post,
		(ContentType::Chats, true) => folder.folder_per_paid_message,
		(ContentType::Chats, false) => folder.folder_per_message,
		_ => false,
	}
}

/// Render a custom filename (`File{}` block) for one media item.
/// Returns `None` when no format is configured for this content type or the
/// rendered name ends up empty (caller should fall back to the default).
///
/// Placeholders (matching the OFDL docs): `{id}`, `{postedAt}`/`{createdAt}`,
/// `{mediaId}`, `{mediaCreatedAt}`, `{filename}`, `{username}`, `{text}`,
/// `{rawText}`. Matching is case-insensitive; unknown placeholders are left
/// as-is.
#[allow(clippy::too_many_arguments)]
pub fn format_download_filename(
	content_type: ContentType,
	is_paid: bool,
	content_id: u64,
	content_timestamp: DateTime<Utc>,
	text: &str,
	username: &str,
	media: &Feed,
	original_filename: &str,
) -> Option<String> {
	let template = match (content_type, is_paid) {
		(ContentType::Posts, true) => config().file.paid_post_file_name_format.as_deref(),
		(ContentType::Posts, false) => config().file.post_file_name_format.as_deref(),
		(ContentType::Chats, true) => config().file.paid_message_file_name_format.as_deref(),
		(ContentType::Chats, false) => config().file.message_file_name_format.as_deref(),
		_ => None,
	};
	let template = template?;
	if template.trim().is_empty() {
		return None;
	}

	let mut fields: HashMap<&str, String> = HashMap::new();
	fields.insert("id", content_id.to_string());
	let date_key = if content_type == ContentType::Chats { "createdAt" } else { "postedAt" };
	fields.insert(date_key, content_timestamp.format("%Y-%m-%d").to_string());
	fields.insert("mediaId", media.id.to_string());
	if let Some(dt) = DateTime::<Utc>::from_timestamp(media.unix_time(), 0) {
		fields.insert("mediaCreatedAt", dt.format("%Y-%m-%d").to_string());
	}
	fields.insert("filename", original_filename.to_string());
	fields.insert("username", username.to_string());
	fields.insert("text", sanitize_text(text));
	fields.insert("rawText", text.to_string());

	let rendered = render_template(template, &fields);
	let rendered = rendered.trim();
	if rendered.is_empty() {
		return None;
	}

	// Keep the original extension unless the template already carried it in
	// via `{filename}`.
	let ext = original_filename
		.rsplit('.')
		.next()
		.filter(|e| !e.is_empty() && e.len() <= 5)
		.unwrap_or("mp4");
	if rendered.to_lowercase().ends_with(&ext.to_lowercase()) {
		Some(rendered.to_string())
	} else {
		Some(format!("{rendered}.{ext}"))
	}
}

fn render_template(template: &str, fields: &HashMap<&str, String>) -> String {
	let mut out = String::with_capacity(template.len());
	let mut rest = template;
	while let Some(open) = rest.find('{') {
		out.push_str(&rest[..open]);
		rest = &rest[open..];
		if let Some(close) = rest.find('}') {
			let token = &rest[1..close];
			let lower = token.to_lowercase();
			if let Some((_, value)) = fields.iter().find(|(k, _)| k.to_lowercase() == lower) {
				out.push_str(value);
				rest = &rest[close + 1..];
				continue;
			}
		}
		// No closing brace or unknown token — keep it verbatim.
		out.push('{');
		rest = &rest[1..];
	}
	out.push_str(rest);
	out
}

/// Sanitize post/message text for use in a filename: replace path-hostile
/// characters, collapse whitespace, and cap the length. Honors
/// `DisableTextSanitization`.
pub fn sanitize_text(text: &str) -> String {
	if config().download.disable_text_sanitization {
		return text.to_string();
	}
	let cleaned: String = text
		.chars()
		.map(|c| {
			if matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') {
				'_'
			} else {
				c
			}
		})
		.collect();
	let collapsed: String = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
	let mut result = collapsed.trim().to_string();
	if result.len() > 80 {
		result.truncate(80);
	}
	result
}

#[cfg(test)]
mod tests {
	use super::*;

	fn fields() -> HashMap<&'static str, String> {
		let mut f = HashMap::new();
		f.insert("id", "42".to_string());
		f.insert("postedAt", "2023-07-30".to_string());
		f.insert("mediaId", "7".to_string());
		f.insert("filename", "abc_source.mp4".to_string());
		f.insert("username", "creator".to_string());
		f.insert("text", "hello world".to_string());
		f
	}

	#[test]
	fn render_known_fields_case_insensitive() {
		let rendered = render_template("{ID}_{postedAt}_{MediaId}_{filename}", &fields());
		assert_eq!(rendered, "42_2023-07-30_7_abc_source.mp4");
	}

	#[test]
	fn render_unknown_token_left_verbatim() {
		let rendered = render_template("{id} + {bogus} + {mediaId}", &fields());
		assert_eq!(rendered, "42 + {bogus} + 7");
	}

	#[test]
	fn render_unclosed_brace_verbatim() {
		let rendered = render_template("a{id}b{unclosed", &fields());
		assert_eq!(rendered, "a42b{unclosed");
	}

	#[test]
	fn render_with_extension_already_present() {
		let out = render_template("pre_{filename}_suffix", &fields());
		assert!(out.ends_with("abc_source.mp4_suffix"));
	}

	#[test]
	fn example_config_parses_into_expected_keys() {
		use std::io::Write;

		let dir = std::env::temp_dir().join("of-scraper-rs-config-test");
		std::fs::create_dir_all(&dir).unwrap();
		let path = dir.join("config.conf");
		let mut f = std::fs::File::create(&path).unwrap();
		f.write_all(include_str!("../config.conf-example").as_bytes()).unwrap();

		let parsed = crate::CustomConfig::load(&path).unwrap();
		// The exact section/key pairs AppConfig::load uses must resolve against
		// the shipped example file, or settings silently stop working.
		assert_eq!(parsed.get("download.media", "downloadimages").map(|s| s.as_str()), Some("true"));
		assert_eq!(parsed.get("download.media", "downloadpaidmessages").map(|s| s.as_str()), Some("true"));
		assert_eq!(parsed.get("download", "downloadpath").map(|s| s.as_str()), Some(""));
		assert_eq!(parsed.get("download", "downloaddateselection").map(|s| s.as_str()), Some("before"));
		assert_eq!(parsed.get("file", "postfilenameformat").map(|s| s.as_str()), Some(""));
		assert_eq!(parsed.get("folder", "folderperpost").map(|s| s.as_str()), Some("false"));
		assert_eq!(parsed.get("subscriptions", "includeexpiredsubscriptions").map(|s| s.as_str()), Some("false"));
		assert_eq!(parsed.get("external", "ffmpegpath").map(|s| s.as_str()), Some(""));
		assert_eq!(parsed.get("performance", "downloadlimitinmbpersec").map(|s| s.as_str()), Some("4"));
		assert_eq!(parsed.get("logging", "logginglevel").map(|s| s.as_str()), Some("error"));
		assert_eq!(parsed.get("download", "ignoreownmessages").map(|s| s.as_str()), Some("false"));
		assert_eq!(parsed.get("download", "downloadduplicatedmedia").map(|s| s.as_str()), Some("false"));
		assert_eq!(parsed.get("download", "skipads").map(|s| s.as_str()), Some("false"));
		assert_eq!(parsed.get("metadata", "saveuserdb").map(|s| s.as_str()), Some("false"));
	}
}
