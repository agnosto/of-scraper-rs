//! Per-creator metadata database, mirroring DIGI's `user_data.db` format
//! (`OnlyFans-7.6.1`-style `<root>/Metadata/user_data.db`), so external
//! tools that read that schema work with files produced by this scraper.
//!
//! Layout: `<download_path>/<username>/Metadata/user_data.db` with the same
//! tables and column vocabulary DIGI uses — `posts`/`messages` (+ unused
//! `stories`/`products`/`others` placeholders), `medias` keyed by `media_id`,
//! `profiles`, and the `alembic_version` marker row importers expect.
//!
//! Capture happens at *scrape* time (see `queue_downloads`), before the
//! download-dedupe checks, so a `--metadata-only` run (or a normal run with
//! `Metadata.SaveUserDb` on) records every content item and media it walks —
//! including media that gets skipped from downloading. Downloads later flip
//! `medias.downloaded`/`size` via [`mark_downloaded`].
//!
//! `directory` intentionally mirrors *our* folder layout (relative to the
//! download path, `/`-separated): tools matching files do so through
//! `directory` + `filename`, so they must contain real file locations.
//!
//! All functions are best-effort: an unwritable database is logged by the
//! caller and skipped, never fatal to the scrape.

use std::{collections::HashMap, fs, path::{Path, PathBuf}, sync::Mutex};

use anyhow::Context;
use chrono::{DateTime, Utc};
use log::warn;
use rusqlite::{params, Connection};
use of_client::media::MediaType;

/// The version DIGI 7.6.1 leaves in `alembic_version`; some readers check it.
const SCHEMA_VERSION: &str = "37c4f2719d65";

/// `MediaType` → the media-type words DIGI's db uses (`Images`/`Videos`/
/// `Audios`). GIF files land in our `Videos` folder, so they mirror that.
pub fn media_type_str(media_type: MediaType) -> &'static str {
	match media_type {
		MediaType::Photo => "Images",
		MediaType::Video | MediaType::Gif => "Videos",
		MediaType::Audio => "Audios",
	}
}

/// `media_dir` relative to the download path, `/`-separated, matching the
/// `directory` column style DIGI writes.
pub fn relative_download_directory(download_path: &Path, media_dir: &Path) -> String {
	let relative = media_dir.strip_prefix(download_path).unwrap_or(media_dir);
	relative.to_string_lossy().replace(std::path::MAIN_SEPARATOR, "/")
}

/// One `posts`/`messages` row. `api_type` decides the table: `Messages` goes
/// to `messages` (with `user_id`), everything else is a `posts` row —
/// matching DIGI, where only `medias` carries the api-type string.
pub struct ContentMeta<'a> {
	pub content_id: u64,
	pub text: &'a str,
	pub price: Option<f32>,
	pub paid: bool,
	pub archived: bool,
	pub created_at: DateTime<Utc>,
	pub api_type: &'a str,
	/// Post author / message sender id (`messages.user_id`).
	pub user_id: Option<u64>,
	/// Username for the `profiles` row (empty → row skipped).
	pub username: &'a str,
}

/// One `medias` row.
pub struct MediaMeta<'a> {
	pub media_id: u64,
	pub post_id: u64,
	pub link: Option<&'a str>,
	pub directory: String,
	pub filename: String,
	pub size: Option<i64>,
	pub api_type: &'a str,
	pub media_type: &'a str,
	/// True when this media is still locked — only the preview is visible,
	/// matching how DIGI's dbs use the column.
	pub preview: bool,
	pub downloaded: bool,
	pub created_at: DateTime<Utc>,
}

/// `Metadata.SaveUserDb` on, or a `--metadata-only` run — those imply capture
/// even without the config key, otherwise a metadata run would record nothing.
pub fn capture_enabled() -> bool {
	crate::config::config().metadata.save_user_db || crate::config::metadata_only()
}

struct MediaDigDb {
	conn: Connection,
}

impl MediaDigDb {
	fn open(path: &Path) -> anyhow::Result<Self> {
		if let Some(parent) = path.parent() {
			fs::create_dir_all(parent).with_context(|| format!("Could not create metadata directory {}", parent.display()))?;
		}
		let conn = Connection::open(path)
			.with_context(|| format!("Could not open metadata db {}", path.display()))?;
		conn.execute_batch(
			"CREATE TABLE IF NOT EXISTS alembic_version (
				version_num VARCHAR(32) NOT NULL,
				CONSTRAINT alembic_version_pkc PRIMARY KEY (version_num)
			);
			CREATE TABLE IF NOT EXISTS medias (
				id INTEGER NOT NULL,
				media_id INTEGER,
				post_id INTEGER NOT NULL,
				link VARCHAR,
				directory VARCHAR,
				filename VARCHAR,
				size INTEGER,
				api_type VARCHAR,
				media_type VARCHAR,
				preview INTEGER,
				linked VARCHAR,
				downloaded INTEGER,
				created_at TIMESTAMP,
				PRIMARY KEY (id),
				UNIQUE (media_id)
			);
			CREATE TABLE IF NOT EXISTS messages (
				id INTEGER NOT NULL,
				post_id INTEGER NOT NULL,
				text VARCHAR,
				price INTEGER,
				paid INTEGER,
				archived BOOLEAN,
				created_at TIMESTAMP,
				user_id INTEGER,
				PRIMARY KEY (id),
				UNIQUE (post_id)
			);
			CREATE TABLE IF NOT EXISTS posts (
				id INTEGER NOT NULL,
				post_id INTEGER NOT NULL,
				text VARCHAR,
				price INTEGER,
				paid INTEGER,
				archived BOOLEAN,
				created_at TIMESTAMP,
				PRIMARY KEY (id),
				UNIQUE (post_id)
			);
			CREATE TABLE IF NOT EXISTS stories (
				id INTEGER NOT NULL,
				post_id INTEGER NOT NULL,
				text VARCHAR,
				price INTEGER,
				paid INTEGER,
				archived BOOLEAN,
				created_at TIMESTAMP,
				PRIMARY KEY (id),
				UNIQUE (post_id)
			);
			CREATE TABLE IF NOT EXISTS products (
				id INTEGER NOT NULL,
				post_id INTEGER NOT NULL,
				text VARCHAR,
				price INTEGER,
				paid INTEGER,
				archived BOOLEAN,
				created_at TIMESTAMP,
				title VARCHAR,
				PRIMARY KEY (id),
				UNIQUE (post_id)
			);
			CREATE TABLE IF NOT EXISTS others (
				id INTEGER NOT NULL,
				post_id INTEGER NOT NULL,
				text VARCHAR,
				price INTEGER,
				paid INTEGER,
				archived BOOLEAN,
				created_at TIMESTAMP,
				PRIMARY KEY (id),
				UNIQUE (post_id)
			);
			CREATE TABLE IF NOT EXISTS profiles (
				id INTEGER NOT NULL,
				user_id INTEGER NOT NULL,
				username VARCHAR NOT NULL,
				PRIMARY KEY (id),
				UNIQUE (username)
			);",
		)
		.with_context(|| format!("Could not initialize metadata schema in {}", path.display()))?;
		conn.execute("INSERT OR REPLACE INTO alembic_version (version_num) VALUES (?1)", params![SCHEMA_VERSION])?;
		Ok(Self { conn })
	}

	fn record_content(&self, meta: &ContentMeta) -> anyhow::Result<()> {
		let created_at = format("%Y-%m-%d %H:%M:%S%.6f", meta.created_at);
		// OF prices are always shown with 2 decimals; the f32 API value needs
		// the decimal round-trip to widen exactly to the 9.99 DIGI's dbs show.
		let price = meta
			.price
			.map(|value| format!("{:.2}", value).parse::<f64>().unwrap_or(0.0))
			.unwrap_or(0.0);
		let paid = meta.paid as i64;
		let archived = meta.archived as i64;

		if meta.api_type == "Messages" {
			self.conn.execute(
				"INSERT INTO messages (post_id, text, price, paid, archived, created_at, user_id)
				 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
				 ON CONFLICT(post_id) DO UPDATE SET
					text = excluded.text,
					price = excluded.price,
					paid = excluded.paid,
					archived = excluded.archived,
					created_at = excluded.created_at,
					user_id = excluded.user_id",
				params![i64::try_from(meta.content_id)?, meta.text, price, paid, archived, created_at, meta.user_id.map(i64::try_from).transpose()?],
			)?;
		} else {
			self.conn.execute(
				"INSERT INTO posts (post_id, text, price, paid, archived, created_at)
				 VALUES (?1, ?2, ?3, ?4, ?5, ?6)
				 ON CONFLICT(post_id) DO UPDATE SET
					text = excluded.text,
					price = excluded.price,
					paid = excluded.paid,
					archived = excluded.archived,
					created_at = excluded.created_at",
				params![i64::try_from(meta.content_id)?, meta.text, price, paid, archived, created_at],
			)?;
		}

		if meta.user_id.is_some() && !meta.username.is_empty() {
			self.conn.execute(
				"INSERT INTO profiles (user_id, username) VALUES (?1, ?2)
				 ON CONFLICT(username) DO UPDATE SET user_id = excluded.user_id",
				params![meta.user_id.map(i64::try_from).transpose()?, meta.username],
			)?;
		}
		Ok(())
	}

	fn record_media(&self, meta: &MediaMeta) -> anyhow::Result<()> {
		let created_at = format("%Y-%m-%d %H:%M:%S%.6f", meta.created_at);
		self.conn.execute(
			"INSERT INTO medias
			 (media_id, post_id, link, directory, filename, size, api_type, media_type, preview, linked, downloaded, created_at)
			 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, NULL, ?10, ?11)
			 ON CONFLICT(media_id) DO UPDATE SET
				post_id = excluded.post_id,
				link = excluded.link,
				directory = excluded.directory,
				filename = excluded.filename,
				size = CASE WHEN excluded.size IS NOT NULL AND excluded.size != 0 THEN excluded.size ELSE size END,
				api_type = excluded.api_type,
				media_type = excluded.media_type,
				preview = excluded.preview,
				downloaded = MAX(downloaded, excluded.downloaded),
				created_at = excluded.created_at",
			params![
				i64::try_from(meta.media_id)?,
				i64::try_from(meta.post_id)?,
				meta.link,
				meta.directory,
				meta.filename,
				meta.size,
				meta.api_type,
				meta.media_type,
				meta.preview as i64,
				meta.downloaded as i64,
				created_at
			],
		)?;
		Ok(())
	}

	fn mark_downloaded(&self, media_id: u64, size: Option<i64>) -> anyhow::Result<()> {
		self.conn.execute(
			"UPDATE medias SET downloaded = 1,
			 size = CASE WHEN ?2 IS NOT NULL AND ?2 != 0 THEN ?2 ELSE size END
			 WHERE media_id = ?1",
			params![i64::try_from(media_id)?, size],
		)?;
		Ok(())
	}
}

/// One open media db per creator, reused across content items.
static DBS: Mutex<Option<HashMap<PathBuf, MediaDigDb>>> = Mutex::new(None);

fn with_creator_db<T>(
	download_path: &Path,
	username: &str,
	f: impl FnOnce(&MediaDigDb) -> anyhow::Result<T>,
) -> anyhow::Result<T> {
	let db_path = download_path.join(username).join("Metadata").join("user_data.db");
	let mut guard = DBS.lock().unwrap();
	let map = guard.get_or_insert_with(HashMap::new);
	let db = match map.get_mut(&db_path) {
		Some(db) => db,
		None => {
			map.insert(db_path.clone(), MediaDigDb::open(&db_path)?);
			map.get_mut(&db_path).expect("just inserted")
		}
	};
	f(db)
}

/// Entry point for the scrape loop: capture one content row (+ profile).
pub fn record_content(download_path: &Path, username: &str, meta: ContentMeta) {
	let dbs_path = download_path.join(username).join("Metadata").join("user_data.db");
	if let Err(e) = with_creator_db(download_path, username, |db| db.record_content(&meta)) {
		warn!("Metadata capture: could not record content {} in {}: {}", meta.content_id, dbs_path.display(), e);
	}
}

/// Entry point for the scrape loop: capture one media row.
pub fn record_media(download_path: &Path, username: &str, meta: MediaMeta) {
	let dbs_path = download_path.join(username).join("Metadata").join("user_data.db");
	if let Err(e) = with_creator_db(download_path, username, |db| db.record_media(&meta)) {
		warn!("Metadata capture: could not record media {} in {}: {}", meta.media_id, dbs_path.display(), e);
	}
}

/// Post-download bookkeeping: mark the media as downloaded, with its size.
pub fn mark_downloaded(download_path: &Path, username: &str, media_id: u64, size: Option<i64>) {
	if let Err(e) = with_creator_db(download_path, username, |db| db.mark_downloaded(media_id, size)) {
		warn!("Metadata capture: could not mark media {} downloaded: {}", media_id, e);
	}
}

/// chrono's format strings can't take a `DateTime` by value; writing the
/// argument order here once keeps the DIGI timestamp format (`… 17:31:46.000000`)
/// in one place.
fn format(pattern: &str, timestamp: DateTime<Utc>) -> String {
	timestamp.format(pattern).to_string()
}

#[cfg(test)]
mod tests {
	use super::*;
	use std::sync::atomic::{AtomicU64, Ordering};

	fn temp_db() -> PathBuf {
		static COUNTER: AtomicU64 = AtomicU64::new(0);
		let n = COUNTER.fetch_add(1, Ordering::Relaxed);
		let path = std::env::temp_dir().join(format!("of-scraper-rs-metadata-test-{}-{}.db", std::process::id(), n));
		let _ = fs::remove_file(&path);
		path
	}

	fn at(when: &str) -> DateTime<Utc> {
		when.parse().unwrap()
	}

	#[test]
	fn schema_and_rows_round_trip() {
		let path = temp_db();
		let db = MediaDigDb::open(&path).unwrap();

		db.record_content(&ContentMeta {
			content_id: 350688755,
			text: "hello post",
			price: None,
			paid: false,
			archived: false,
			created_at: at("2022-06-27T17:27:20Z"),
			api_type: "Posts",
			user_id: Some(186389633),
			username: "bunni.emmie",
		})
		.unwrap();

		db.record_content(&ContentMeta {
			content_id: 630298328406,
			text: "a message",
			price: Some(9.99),
			paid: true,
			archived: false,
			created_at: at("2022-06-27T17:31:46Z"),
			api_type: "Messages",
			user_id: Some(186389633),
			username: "bunni.emmie",
		})
		.unwrap();

		db.record_media(&MediaMeta {
			media_id: 2501528150,
			post_id: 350123342,
			link: Some("https://cdn2.onlyfans.com/files/a/a9/x.jpg"),
			directory: "bunni.emmie/Posts/Images".to_string(),
			filename: "xx.jpg".to_string(),
			size: None,
			api_type: "Posts",
			media_type: "Images",
			preview: false,
			downloaded: false,
			created_at: at("2022-06-26T17:45:15Z"),
		})
		.unwrap();

		let content_row = db.conn
			.query_row("SELECT post_id, text, price, paid FROM posts", [], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?, row.get::<_, f64>(2)?, row.get::<_, i64>(3)?)))
			.unwrap();
		assert_eq!(content_row, (350688755, "hello post".into(), 0.0, 0));

		let message_row = db.conn
			.query_row("SELECT post_id, price, paid, user_id FROM messages", [], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, f64>(1)?, row.get::<_, i64>(2)?, row.get::<_, i64>(3)?)))
			.unwrap();
		assert_eq!(message_row, (630298328406, 9.99, 1, 186389633));

		let media_row = db.conn
			.query_row("SELECT filename, directory, downloaded, created_at FROM medias", [], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, i64>(2)?, row.get::<_, String>(3)?)))
			.unwrap();
		assert_eq!(media_row.2, 0);
		assert_eq!(media_row.1, "bunni.emmie/Posts/Images");
		assert_eq!(media_row.3, "2022-06-26 17:45:15.000000"); // DIGI timestamp format

		let profile = db.conn
			.query_row("SELECT user_id, username FROM profiles", [], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)))
			.unwrap();
		assert_eq!(profile, (186389633, "bunni.emmie".to_string()));

		let version: String = db.conn.query_row("SELECT version_num FROM alembic_version", [], |row| row.get(0)).unwrap();
		assert_eq!(version, SCHEMA_VERSION);

		let _ = fs::remove_file(&path);
	}

	/// Record a row twice — the DIGI tables carry UNIQUE keys, so capture is
	/// upsert-only: text refreshes, and `downloaded`/`size` never regress.
	#[test]
	fn upserts_never_create_duplicates_or_regress() {
		let path = temp_db();
		let db = MediaDigDb::open(&path).unwrap();

		let content = |text: &'static str| ContentMeta {
			content_id: 350688755,
			text,
			price: None,
			paid: false,
			archived: false,
			created_at: at("2022-06-27T17:27:20Z"),
			api_type: "Posts",
			user_id: None,
			username: "",
		};
		db.record_content(&content("old")).unwrap();
		db.record_content(&content("updated")).unwrap();
		let count: i64 = db.conn.query_row("SELECT COUNT(*) FROM posts", [], |row| row.get(0)).unwrap();
		assert_eq!(count, 1);
		let text: String = db.conn.query_row("SELECT text FROM posts", [], |row| row.get(0)).unwrap();
		assert_eq!(text, "updated");

		let media = |downloaded: bool, size: Option<i64>| MediaMeta {
			media_id: 2501528150,
			post_id: 350123342,
			link: None,
			directory: "Posts/Images".to_string(),
			filename: "x.jpg".to_string(),
			size,
			api_type: "Posts",
			media_type: "Images",
			preview: false,
			downloaded,
			created_at: at("2022-06-26T17:45:15Z"),
		};
		db.record_media(&media(true, Some(320489))).unwrap();
		// A later metadata-only pass still sees the row but must not reset it.
		db.record_media(&media(false, None)).unwrap();

		// A real download success marks a not-yet-downloaded row and writes
		// the true file size over the recorded one.
		db.record_media(&media(false, None)).unwrap();
		db.mark_downloaded(2501528150, Some(400)).unwrap();
		let state = db.conn
			.query_row("SELECT downloaded, size FROM medias", [], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, Option<i64>>(1)?)))
			.unwrap();
		assert_eq!(state, (1, Some(400)));
		// Size only grows (or fills in): marking again with a bigger value wins.
		db.mark_downloaded(2501528150, Some(500)).unwrap();
		let size: i64 = db.conn.query_row("SELECT size FROM medias", [], |row| row.get(0)).unwrap();
		assert_eq!(size, 500);

		let count: i64 = db.conn.query_row("SELECT COUNT(*) FROM medias", [], |row| row.get(0)).unwrap();
		assert_eq!(count, 1);
		let _ = fs::remove_file(&path);
	}

	/// Locked media without a username must not poison `profiles` with an
	/// empty or NULL username.
	#[test]
	fn empty_profile_username_is_skipped() {
		let path = temp_db();
		let db = MediaDigDb::open(&path).unwrap();
		db.record_content(&ContentMeta {
			content_id: 1,
			text: "t",
			price: None,
			paid: false,
			archived: false,
			created_at: at("2022-06-27T17:27:20Z"),
			api_type: "Posts",
			user_id: Some(9),
			username: "",
		})
		.unwrap();
		let count: i64 = db.conn.query_row("SELECT COUNT(*) FROM profiles", [], |row| row.get(0)).unwrap();
		assert_eq!(count, 0);
		let _ = fs::remove_file(&path);
	}

	#[test]
	fn media_type_words_match_digi_vocabulary() {
		assert_eq!(media_type_str(MediaType::Photo), "Images");
		assert_eq!(media_type_str(MediaType::Gif), "Videos"); // GIFs land in our Videos dir
		assert_eq!(media_type_str(MediaType::Video), "Videos");
		assert_eq!(media_type_str(MediaType::Audio), "Audios");
	}

	#[test]
	fn download_directory_is_relative_and_forward_slashed() {
		let download_path = PathBuf::from("/downloads");
		let media_dir = download_path.join("bunni.emmie").join("Posts").join("Free label").join("Images");
		assert_eq!(relative_download_directory(&download_path, &media_dir), "bunni.emmie/Posts/Free label/Images");
		// Not under the download path at all → falls back to the whole path.
		assert_eq!(relative_download_directory(&download_path, Path::new("/elsewhere/x/Images")), "/elsewhere/x/Images");
	}
}
