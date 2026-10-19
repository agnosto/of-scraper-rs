//! Persistent record of downloaded media ids, stored in SQLite in the config
//! directory (same resolution rules as `of-scraper-rs.log`/`auth.json`, so it
//! lands in `~/.config/of-scraper-rs/state.db` on Linux and
//! `%APPDATA%\of-scraper-rs\state.db` on Windows).
//!
//! Two behaviors hang off this:
//! - [`has_media`] lets `Download.SkipExistingMedia` skip media that was
//!   downloaded in a previous run, before even queueing it
//! - [`record`] is called after every successful download, regardless of that
//!   setting, so the store stays complete for `db stats` / `backfill` etc.
//!
//! Every function treats a broken/inaccessible database as empty or a no-op
//! warning — a database problem should never take down a scrape.

use std::{fs, path::PathBuf, sync::Mutex};

use anyhow::{anyhow, Context};
use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, OptionalExtension};

/// One row of the `media` table.
#[derive(Debug, Clone)]
pub struct MediaRecord {
	pub media_id: u64,
	pub creator: String,
	pub media_type: String,
	pub content_type: String,
	pub downloaded_at: DateTime<Utc>,
}

/// Aggregate counts, per creator and overall.
#[derive(Debug, Default)]
pub struct Stats {
	pub total: u64,
	pub oldest: Option<String>,
	pub newest: Option<String>,
	pub by_creator: Vec<CreatorStats>,
}

#[derive(Debug, Default)]
pub struct CreatorStats {
	pub creator: String,
	pub total: u64,
	pub photos: u64,
	pub videos: u64,
	pub audios: u64,
	pub other: u64,
	pub oldest: Option<String>,
	pub newest: Option<String>,
}

/// Single db connection for the run, opened on first use.
static DB: Mutex<Option<MediaDb>> = Mutex::new(None);

/// Where the state database lives.
pub fn path() -> PathBuf {
	crate::resolve_config_path("state.db")
}

/// Run `f` against the database, opening it lazily. Errors are returned for
/// callers to treat as best-effort.
pub fn with_db<T>(f: impl FnOnce(&MediaDb) -> anyhow::Result<T>) -> anyhow::Result<T> {
	let mut guard = DB.lock().map_err(|e| anyhow!("state db mutex poisoned: {e}"))?;
	if guard.is_none() {
		*guard = Some(MediaDb::open(&path())?);
	}
	f(guard.as_ref().unwrap())
}

/// True when the media id is recorded (a previous run downloaded it).
pub fn has_media(media_id: u64) -> anyhow::Result<bool> {
	with_db(|db| db.has_media(media_id))
}

/// Record a successful download. Idempotent: a later re-download does not
/// move the original date, so `oldest`/`newest` stats reflect when files
/// actually first appeared.
pub fn record(record: &MediaRecord) -> anyhow::Result<()> {
	with_db(|db| db.record(record))
}

/// Delete records (all, or one creator's). Returns how many rows went away.
pub fn clear(creator: Option<&str>) -> anyhow::Result<usize> {
	with_db(|db| db.clear(creator))
}

pub fn stats(creator: Option<&str>) -> anyhow::Result<Stats> {
	with_db(|db| db.stats(creator))
}

/// Newest-first slice of recorded downloads.
pub fn list(creator: Option<&str>, limit: u64, offset: u64) -> anyhow::Result<Vec<MediaRecord>> {
	with_db(|db| db.list(creator, limit, offset))
}

pub struct MediaDb {
	conn: Connection,
}

impl MediaDb {
	pub fn open(path: &std::path::Path) -> anyhow::Result<Self> {
		if let Some(parent) = path.parent() {
			fs::create_dir_all(parent)
				.with_context(|| format!("Could not create state db directory {}", parent.display()))?;
		}
		let conn = Connection::open(path)
			.with_context(|| format!("Could not open state db {}", path.display()))?;
		conn.pragma_update(None, "journal_mode", "WAL")
			.context("Could not enable WAL for state db")?;

		conn.execute_batch(
			"CREATE TABLE IF NOT EXISTS media (
				media_id      INTEGER PRIMARY KEY,
				creator       TEXT NOT NULL,
				media_type    TEXT NOT NULL,
				content_type  TEXT NOT NULL,
				downloaded_at TEXT NOT NULL
			);
			CREATE INDEX IF NOT EXISTS idx_media_creator ON media(creator);
			CREATE TABLE IF NOT EXISTS meta (
				key   TEXT PRIMARY KEY,
				value TEXT NOT NULL
			);
			INSERT OR IGNORE INTO meta (key, value) VALUES ('schema_version', '1');",
		)
		.context("Could not initialize state db schema")?;

		Ok(Self { conn })
	}

	pub fn has_media(&self, media_id: u64) -> anyhow::Result<bool> {
		let mut stmt = self.conn.prepare_cached("SELECT 1 FROM media WHERE media_id = ?1")?;
		let found = stmt
			.query_row(params![i64::try_from(media_id)?], |row| row.get::<_, i64>(0))
			.optional()?;
		Ok(found.is_some())
	}

	pub fn record(&self, record: &MediaRecord) -> anyhow::Result<()> {
		self.conn.execute(
			"INSERT INTO media (media_id, creator, media_type, content_type, downloaded_at)
			 VALUES (?1, ?2, ?3, ?4, ?5)
			 ON CONFLICT(media_id) DO NOTHING",
			params![
				i64::try_from(record.media_id)?,
				record.creator,
				record.media_type,
				record.content_type,
				record.downloaded_at.to_rfc3339()
			],
		)?;
		Ok(())
	}

	pub fn clear(&self, creator: Option<&str>) -> anyhow::Result<usize> {
		let rows = match creator {
			Some(c) => self.conn.execute("DELETE FROM media WHERE creator = ?1", params![c])?,
			None => self.conn.execute("DELETE FROM media", params![])?,
		};
		Ok(rows)
	}

	pub fn stats(&self, creator: Option<&str>) -> anyhow::Result<Stats> {
		let mut stats = Stats::default();

		let mut stmt = self.conn.prepare_cached(
			"SELECT COUNT(*), MIN(downloaded_at), MAX(downloaded_at) FROM media
			 WHERE (?1 IS NULL OR creator = ?1)",
		)?;
		stmt.query_row(params![creator], |row| {
			stats.total = row.get::<_, i64>(0)? as u64;
			stats.oldest = row.get(1)?;
			stats.newest = row.get(2)?;
			Ok(())
		})?;

		let mut stmt = self.conn.prepare_cached(
			"SELECT creator, media_type, COUNT(*), MIN(downloaded_at), MAX(downloaded_at)
			 FROM media
			 WHERE (?1 IS NULL OR creator = ?1)
			 GROUP BY creator, media_type
			 ORDER BY creator, media_type",
		)?;
		let mut rows = stmt.query(params![creator])?;
		while let Some(row) = rows.next()? {
			let creator: String = row.get(0)?;
			let media_type: String = row.get(1)?;
			let count: u64 = row.get::<_, i64>(2)? as u64;
			let oldest: Option<String> = row.get(3)?;
			let newest: Option<String> = row.get(4)?;

			let entry = if let Some(existing) = stats.by_creator.iter_mut().find(|s| s.creator == creator) {
				existing
			} else {
				stats.by_creator.push(CreatorStats { creator: creator.clone(), ..Default::default() });
				stats.by_creator.last_mut().unwrap()
			};
			match media_type.as_str() {
				"Photo" => entry.photos += count,
				"Video" => entry.videos += count,
				"Audio" => entry.audios += count,
				_ => entry.other += count,
			}
			entry.oldest = earlier_of(entry.oldest.take(), oldest);
			entry.newest = later_of(entry.newest.take(), newest);
			entry.total += count;
		}

		Ok(stats)
	}

	pub fn list(&self, creator: Option<&str>, limit: u64, offset: u64) -> anyhow::Result<Vec<MediaRecord>> {
		let mut stmt = self
			.conn
			.prepare_cached(
				"SELECT media_id, creator, media_type, content_type, downloaded_at
				 FROM media
				 WHERE (?1 IS NULL OR creator = ?1)
				 ORDER BY downloaded_at DESC, media_id DESC
				 LIMIT ?2 OFFSET ?3",
			)?;
		let mut rows = stmt.query(params![creator, i64::try_from(limit)?, i64::try_from(offset)?])?;
		let mut out = Vec::new();
		while let Some(row) = rows.next()? {
			out.push(MediaRecord {
				media_id: row.get::<_, i64>(0)? as u64,
				creator: row.get(1)?,
				media_type: row.get(2)?,
				content_type: row.get(3)?,
				downloaded_at: row.get::<_, String>(4)?.parse().unwrap_or_else(|_| Utc::now()),
			});
		}
		Ok(out)
	}
}

/// HTML-safe? No — RFC3339 sorts lexicographically for the same UTC offset,
/// so string comparison is equivalent to time comparison here.
fn earlier_of(a: Option<String>, b: Option<String>) -> Option<String> {
	match (a, b) {
		(Some(a), Some(b)) => Some(a.min(b)),
		(a, b) => a.or(b),
	}
}

fn later_of(a: Option<String>, b: Option<String>) -> Option<String> {
	match (a, b) {
		(Some(a), Some(b)) => Some(a.max(b)),
		(a, b) => a.or(b),
	}
}

/// Compact display form for CLI output; falls back to the raw string.
pub fn fmt_timestamp(timestamp: &str) -> String {
	DateTime::parse_from_rfc3339(timestamp)
		.map(|dt| dt.format("%Y-%m-%d %H:%M:%S").to_string())
		.unwrap_or_else(|_| timestamp.to_string())
}

/// mtime of an already-produced file, used (with backfill) as the download date.
pub fn system_time_to_rfc3339(value: std::time::SystemTime) -> String {
	let dt: DateTime<Utc> = value.into();
	dt.to_rfc3339()
}

#[cfg(test)]
mod tests {
	use super::*;
	use std::path::PathBuf;
	use std::sync::atomic::{AtomicU64, Ordering};

	/// Unique temp file per call so parallel tests don't collide.
	fn temp_db() -> PathBuf {
		static COUNTER: AtomicU64 = AtomicU64::new(0);
		let n = COUNTER.fetch_add(1, Ordering::Relaxed);
		let path = std::env::temp_dir().join(format!("of-scraper-rs-db-test-{}-{}.db", std::process::id(), n));
		let _ = fs::remove_file(&path);
		path
	}

	fn sample(id: u64, creator: &str, media_type: &str, content_type: &str, at: &str) -> MediaRecord {
		MediaRecord {
			media_id: id,
			creator: creator.to_string(),
			media_type: media_type.to_string(),
			content_type: content_type.to_string(),
			downloaded_at: at.parse().unwrap(),
		}
	}

	#[test]
	fn records_are_visible_to_has_media_and_survive_reopen() {
		let path = temp_db();
		{
			let db = MediaDb::open(&path).unwrap();
			assert!(!db.has_media(183936505).unwrap());
			db.record(&sample(183936505, "sis.space", "Video", "Posts", "2026-10-19T08:00:00Z")).unwrap();
			assert!(db.has_media(183936505).unwrap());
		}
		// Fresh connection over the same file sees the same row.
		let db = MediaDb::open(&path).unwrap();
		assert!(db.has_media(183936505).unwrap());
		let _ = fs::remove_file(&path);
	}

	#[test]
	fn record_is_idempotent_first_download_wins() {
		let db = MediaDb::open(&temp_db()).unwrap();
		db.record(&sample(42, "sis.space", "Photo", "Posts", "2026-10-19T08:00:00Z")).unwrap();
		// A re-download (--force) doesn't update the first-seen date.
		db.record(&sample(42, "sis.space", "Photo", "Posts", "2027-01-01T00:00:00Z")).unwrap();
		let entry = db.list(Some("sis.space"), 10, 0).unwrap();
		let expected: DateTime<Utc> = "2026-10-19T08:00:00Z".parse().unwrap();
		assert_eq!(entry[0].downloaded_at, expected);
	}

	#[test]
	fn clear_is_scoped_to_creator() {
		let db = MediaDb::open(&temp_db()).unwrap();
		db.record(&sample(1, "a", "Photo", "Posts", "2026-01-01T00:00:00Z")).unwrap();
		db.record(&sample(2, "b", "Video", "Messages", "2026-01-02T00:00:00Z")).unwrap();

		assert_eq!(db.clear(Some("a")).unwrap(), 1);
		assert!(!db.has_media(1).unwrap());
		assert!(db.has_media(2).unwrap()); // untouched

		assert_eq!(db.clear(None).unwrap(), 1);
		assert!(!db.has_media(2).unwrap());
	}

	#[test]
	fn stats_break_down_by_creator_and_media_type() {
		let db = MediaDb::open(&temp_db()).unwrap();
		for (id, creator, kind) in [(1u64, "a", "Photo"), (2, "a", "Photo"), (3, "a", "Video"), (4, "b", "Audio")] {
			let at = if id == 3 { "2026-02-01T00:00:00Z" } else { "2026-05-05T00:00:00Z" };
			db.record(&sample(id, creator, kind, "Posts", at)).unwrap();
		}

		let all = db.stats(None).unwrap();
		assert_eq!(all.total, 4);
		assert_eq!(all.by_creator.len(), 2);

		let a = all.by_creator.iter().find(|s| s.creator == "a").unwrap();
		assert_eq!(a.total, 3);
		assert_eq!(a.photos, 2);
		assert_eq!(a.videos, 1);
		assert_eq!(a.audios, 0);
		// Row-level MIN/MAX feed the per-creator window too.
		assert_eq!(a.oldest.as_deref(), Some("2026-02-01T00:00:00+00:00"));
		assert_eq!(a.newest.as_deref(), Some("2026-05-05T00:00:00+00:00"));

		let b = db.stats(Some("b")).unwrap();
		assert_eq!(b.total, 1);
		assert_eq!(b.by_creator[0].audios, 1);
	}

	#[test]
	fn list_is_newest_first_and_respects_limit_offset() {
		let db = MediaDb::open(&temp_db()).unwrap();
		for (id, at) in [(5, "2027-01-03T00:00:00Z"), (1, "2027-01-01T00:00:00Z"), (9, "2027-01-02T00:00:00Z")] {
			db.record(&sample(id, "a", "Photo", "Posts", at)).unwrap();
		}
		let page = db.list(Some("a"), 2, 1).unwrap();
		assert_eq!(page.len(), 2);
		assert_eq!(page[0].media_id, 9);
		assert_eq!(page[1].media_id, 1);
	}

	#[test]
	fn schema_includes_meta_version() {
		let path = temp_db();
		let db = MediaDb::open(&path).unwrap();
		let version: String = db.conn.query_row("SELECT value FROM meta WHERE key = 'schema_version'", [], |r| r.get(0)).unwrap();
		assert_eq!(version, "1");
		let _ = fs::remove_file(&path);
	}
}
