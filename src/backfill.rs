//! Backfill the download database from files already on disk.
//!
//! Media ids are only recoverable from filenames that were rendered with a
//! `{mediaId}` placeholder (`File.*FileNameFormat`), so those formats drive
//! the whole thing: each configured template becomes a regex by turning
//! `{mediaId}` into a digit-run capture and everything else into loose
//! matches. Default-named files (no `{mediaId}` in the format) can't be
//! matched by id — they report as no-match and keep relying on the
//! `If-Modified-Since` skip the downloader does anyway.
//!
//! Best effort by design: `{text}` renders are sanitization/truncation
//! driven and digit runs can appear in free text, so an extremely loose
//! template can occasionally capture a wrong number. The literal pieces of
//! the template around `{mediaId}` are what make it work in practice
//! (`{id}_{mediaId}_{filename}` etc.).

use std::{fs, path::{Path, PathBuf}};

use anyhow::Context;
use chrono::{DateTime, Utc};
use regex::Regex;

use crate::{config::config, db};

/// Per-creator result of a backfill run.
#[derive(Debug, Default, Clone)]
pub struct CreatorBackfill {
	pub creator: String,
	pub files_seen: u64,
	pub imported: u64,
	pub no_match: u64,
}

impl CreatorBackfill {
	fn new(creator: impl Into<String>) -> Self {
		Self { creator: creator.into(), ..Default::default() }
	}
}

/// Import every already-downloaded file under `download_path` (optionally
/// limited to one creator directory) into the db. Original mtimes stand in
/// for `downloaded_at`. Returns per-creator summaries and does not fail on
/// individual files.
pub fn run(creator: Option<&str>, download_path: &Path) -> anyhow::Result<Vec<CreatorBackfill>> {
	let lookup = MediaIdLookup::from_config();
	let mut summaries = Vec::new();

	for creator_dir in creator_dirs(download_path, creator)? {
		let mut summary = CreatorBackfill::new(
			creator_dir
				.file_name()
				.map(|name| name.to_string_lossy().to_string())
				.unwrap_or_default(),
		);
		if summary.creator.is_empty() {
			continue;
		}

		for (dir_names, path) in walk_files(&creator_dir, 6) {
			summary.files_seen += 1;

			let Some(media_id) = lookup.media_id(&path) else {
				summary.no_match += 1;
				continue;
			};

			let downloaded_at = fs::metadata(&path)
				.and_then(|meta| meta.modified())
				.ok()
				.map(db::system_time_to_rfc3339)
				.map(|value| value.parse::<DateTime<Utc>>().unwrap_or_else(|_| Utc::now()))
				.unwrap_or_else(Utc::now);

			let record = db::MediaRecord {
				media_id,
				creator: summary.creator.clone(),
				media_type: media_type_for(&dir_names),
				content_type: content_type_for(&dir_names),
				downloaded_at,
			};

			if let Err(e) = db::record(&record) {
				summary.no_match += 1;
				eprintln!("db backfill: could not record {}: {}", path.display(), e);
				continue;
			}
			summary.imported += 1;
		}

		summaries.push(summary);
	}

	Ok(summaries)
}

/// Creator directories: any child directory of the download root, optionally
/// filtered to one creator.
fn creator_dirs(download_path: &Path, creator: Option<&str>) -> anyhow::Result<Vec<PathBuf>> {
	if let Some(creator) = creator {
		let dir = download_path.join(creator);
		if !dir.is_dir() {
			return Err(anyhow::anyhow!(
				"{} does not exist (under download path {})",
				dir.display(),
				download_path.display()
			));
		}
		return Ok(vec![dir]);
	}

	let mut out = Vec::new();
	for entry in fs::read_dir(download_path).with_context(|| format!("Reading {}", download_path.display()))? {
		let entry = entry?;
		if entry.file_type()?.is_dir() {
			out.push(entry.path());
		}
	}
	out.sort();
	Ok(out)
}

/// Depth-limited file walk. With each file comes the list of ancestor
/// directory names (`[creator, content dir, media dir, ...]`), which the
/// media/content-type classification keys off.
fn walk_files(dir: &Path, depth: u8) -> Vec<(Vec<String>, PathBuf)> {
	let mut out = Vec::new();
	if depth == 0 {
		return out;
	}
	let Ok(entries) = fs::read_dir(dir) else {
		return out;
	};

	for entry in entries.flatten() {
		let path = entry.path();
		if path.is_dir() {
			let dir_name = path.file_name().map(|name| name.to_string_lossy().to_string()).unwrap_or_default();
			for (mut ancestors, file) in walk_files(&path, depth - 1) {
				ancestors.insert(0, dir_name.clone());
				out.push((ancestors, file));
			}
		} else {
			out.push((Vec::new(), path));
		}
	}
	out
}

/// `Images` dir -> Photo, `Videos` -> Video, `Audios` -> Audio.
fn media_type_for(dir_names: &[String]) -> String {
	if dir_names.iter().any(|d| d == "Audios") {
		"Audio".to_string()
	} else if dir_names.iter().any(|d| d == "Videos") {
		"Video".to_string()
	} else if dir_names.iter().any(|d| d == "Images") {
		"Photo".to_string()
	} else {
		"Other".to_string()
	}
}

/// Content dir name (`Posts`, `Messages`, ...) recorded as-is; empty when no
/// known content directory appears among the ancestors.
fn content_type_for(dir_names: &[String]) -> String {
	dir_names
		.iter()
		.find(|d| {
			matches!(
				d.as_str(),
				"Posts" | "Messages" | "Stories" | "Highlights" | "Purchases" | "Labels" | "Streams" | "Notifications"
			)
		})
		.cloned()
		.unwrap_or_default()
}

/// The configured filename-format templates as `{mediaId}` regexes.
struct MediaIdLookup {
	patterns: Vec<Regex>,
}

impl MediaIdLookup {
	fn from_config() -> Self {
		let formats = &config().file;
		let templates = [
			formats.post_file_name_format.as_deref(),
			formats.paid_post_file_name_format.as_deref(),
			formats.message_file_name_format.as_deref(),
			formats.paid_message_file_name_format.as_deref(),
		];
		let mut patterns = Vec::new();
		for template in templates.into_iter().flatten() {
			if let Some(regex_src) = media_id_regex(template)
				&& let Ok(regex) = Regex::new(&regex_src)
			{
				patterns.push(regex);
			}
		}
		Self { patterns }
	}

	/// The captured media id, if any template matches.
	fn media_id(&self, path: &Path) -> Option<u64> {
		let filename = path.file_name()?.to_string_lossy();
		self.patterns
			.iter()
			.find_map(|regex| regex.captures(&filename))
			.and_then(|caps| caps.get(1))
			.and_then(|m| m.as_str().parse::<u64>().ok())
	}
}

/// Build the regex source for one template. `None` when the template has no
/// `{mediaId}` or its neighbors are too ambiguous to match reliably.
fn media_id_regex(template: &str) -> Option<String> {
	const TOKEN: &str = "{mediaid}";
	let token_pos = template.to_lowercase().find(TOKEN)?;

	// Literal between the previous placeholder and `{mediaId}`.
	let before = &template[..token_pos];
	let prefix = match before.rfind('}') {
		Some(close) => {
			let literal = &before[close + 1..];
			if literal.is_empty() || literal.contains('{') {
				// Adjacent placeholders (`{id}{mediaId}`) give nothing to
				// anchor the digit run on — a bare capture would grab the
				// post id instead. Side benefit: regexes stay unambiguous.
				return None;
			}
			literal
		}
		None => before,
	};

	// Literal between `{mediaId}` and the next placeholder/end.
	let after = &template[token_pos + TOKEN.len()..];
	let suffix_len = after.find(['{', '}']).unwrap_or(after.len());
	let suffix = &after[..suffix_len];

	Some(format!("{}(\\d{{6,}}){}", regex::escape(prefix), regex::escape(suffix)))
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn template_regex_captures_media_id() {
		let regex = Regex::new(&media_id_regex("{id}_{mediaId}_{filename}").unwrap()).unwrap();
		let caps = regex.captures("15846235_183936505_x.mp4").unwrap();
		assert_eq!(caps.get(1).unwrap().as_str(), "183936505");
	}

	/// Zero-width look-around plus anchors keep `{mediaId}` ambiguities
	/// resolvable when it is at template start.
	#[test]
	fn template_regex_handles_leading_token() {
		let regex = Regex::new(&media_id_regex("{mediaId}_{filename}").unwrap()).unwrap();
		let caps = regex.captures("183936505_source.mp4").unwrap();
		assert_eq!(caps.get(1).unwrap().as_str(), "183936505");
	}

	/// Trailing token: the renderer appends the extension, so the capture
	/// must land before `.ext`.
	#[test]
	fn template_regex_handles_trailing_token() {
		let regex = Regex::new(&media_id_regex("{filename}_{mediaId}").unwrap()).unwrap();
		let caps = regex.captures("some_photo_183936505.jpg").unwrap();
		assert_eq!(caps.get(1).unwrap().as_str(), "183936505");
	}

	#[test]
	fn template_regex_rejects_unrecoverable_shapes() {
		// No `{mediaId}` — default filenames can't be backfilled.
		assert!(media_id_regex("{filename}").is_none());
		// Adjacent placeholders: nothing anchors the capture.
		assert!(media_id_regex("{id}{mediaId}").is_none());
	}

	/// Empty lookup (no `{mediaId}` formats configured — the default) reports
	/// every file as a no-match.
	#[test]
	fn empty_lookup_never_matches() {
		let lookup = MediaIdLookup { patterns: Vec::new() };
		assert!(lookup.media_id(Path::new("whatever_source.mp4")).is_none());
	}
}
