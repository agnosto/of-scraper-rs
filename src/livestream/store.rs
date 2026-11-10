//! Per-creator livestream metadata storage in `<creator>/Metadata/user_data.db`
//! (`livestreams` and `livestream_recordings` tables), mirroring the SubScraper
//! schema. All functions are best-effort: a database problem must never take
//! down a recording.

use std::path::Path;

use chrono::{DateTime, Utc};
use rusqlite::{params, Connection};

pub struct LivestreamRecord {
	pub stream_id: u64,
	pub text: String,
	pub raw_text: String,
	pub price: i64,
	pub started_at: Option<DateTime<Utc>>,
	pub finished_at: Option<DateTime<Utc>>,
	pub duration_secs: Option<i64>,
	pub views_count: u64,
	pub likes_count: u64,
	pub comments_count: u64,
}

pub struct RecordingRecord {
	pub stream_id: u64,
	pub part: u32,
	pub directory: String,
	pub filename: String,
	pub size: u64,
	pub duration_ms: i64,
	pub recording_started_at: DateTime<Utc>,
	pub recording_finished_at: DateTime<Utc>,
	pub end_reason: String,
	pub format: String,
}

fn db_path(creator_path: &Path) -> std::path::PathBuf {
	creator_path.join("Metadata").join("user_data.db")
}

fn open(creator_path: &Path) -> rusqlite::Result<Connection> {
	std::fs::create_dir_all(creator_path.join("Metadata")).ok();
	let conn = Connection::open(db_path(creator_path))?;
	conn.execute_batch(
		"CREATE TABLE IF NOT EXISTS livestreams (id INTEGER NOT NULL, stream_id INTEGER NOT NULL, text VARCHAR, raw_text VARCHAR, price INTEGER, started_at TIMESTAMP, finished_at TIMESTAMP, duration INTEGER, views_count INTEGER, likes_count INTEGER, comments_count INTEGER, record_created_at TIMESTAMP, record_updated_at TIMESTAMP, PRIMARY KEY(id), UNIQUE(stream_id));
		 CREATE TABLE IF NOT EXISTS livestream_recordings (id INTEGER NOT NULL, stream_id INTEGER NOT NULL, part INTEGER NOT NULL, directory VARCHAR, filename VARCHAR, size INTEGER, duration INTEGER, recording_started_at TIMESTAMP, recording_finished_at TIMESTAMP, end_reason VARCHAR, format VARCHAR, record_created_at TIMESTAMP, PRIMARY KEY(id));",
	)?;
	Ok(conn)
}

fn timestamp(value: Option<DateTime<Utc>>) -> Option<String> {
	value.map(|v| v.to_rfc3339())
}

/// One row per broadcast, refreshed while it is recorded.
pub fn upsert_livestream(creator_path: &Path, record: &LivestreamRecord) {
	let result = open(creator_path).and_then(|conn| {
		let now = Utc::now().to_rfc3339();
		conn.execute(
			"INSERT INTO livestreams(stream_id, text, raw_text, price, started_at, finished_at, duration, views_count, likes_count, comments_count, record_created_at, record_updated_at)
			 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?11)
			 ON CONFLICT(stream_id) DO UPDATE SET
				text = excluded.text, raw_text = excluded.raw_text, price = excluded.price,
				started_at = COALESCE(excluded.started_at, livestreams.started_at),
				finished_at = COALESCE(excluded.finished_at, livestreams.finished_at),
				duration = COALESCE(excluded.duration, livestreams.duration),
				views_count = MAX(COALESCE(excluded.views_count, 0), COALESCE(livestreams.views_count, 0)),
				likes_count = MAX(COALESCE(excluded.likes_count, 0), COALESCE(livestreams.likes_count, 0)),
				comments_count = MAX(COALESCE(excluded.comments_count, 0), COALESCE(livestreams.comments_count, 0)),
				record_updated_at = excluded.record_updated_at",
			params![
				record.stream_id as i64,
				record.text,
				record.raw_text,
				record.price,
				timestamp(record.started_at),
				timestamp(record.finished_at),
				record.duration_secs,
				record.views_count as i64,
				record.likes_count as i64,
				record.comments_count as i64,
				now,
			],
		)?;
		Ok(())
	});
	if let Err(e) = result {
		log::warn!("Could not store livestream metadata for {}: {e}", creator_path.display());
	}
}

/// One row per saved file.
pub fn add_recording(creator_path: &Path, record: &RecordingRecord) {
	let result = open(creator_path).and_then(|conn| {
		conn.execute(
			"INSERT INTO livestream_recordings(stream_id, part, directory, filename, size, duration, recording_started_at, recording_finished_at, end_reason, format, record_created_at)
			 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
			params![
				record.stream_id as i64,
				record.part as i64,
				record.directory,
				record.filename,
				record.size as i64,
				record.duration_ms,
				record.recording_started_at.to_rfc3339(),
				record.recording_finished_at.to_rfc3339(),
				record.end_reason,
				record.format,
				Utc::now().to_rfc3339(),
			],
		)?;
		Ok(())
	});
	if let Err(e) = result {
		log::warn!("Could not store livestream recording row for {}: {e}", creator_path.display());
	}
}
