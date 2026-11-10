//! The livestream filename format: its tokens, default, and uniqueness rule.
//! Ported from SubScraper's `LivestreamFileNames`.

use chrono::{DateTime, Local, Utc};
use regex::Regex;

/// Used when no livestream format is configured; matches the recorder's original naming.
pub const DEFAULT_FORMAT: &str = "{username}_{recordedAt}";

const TEXT_LENGTH_LIMIT: usize = 100;

pub const TOKENS: [&str; 6] = ["id", "username", "startedAt", "startedAtTime", "recordedAt", "text"];

const UNIQUE_TOKENS: [&str; 3] = ["{id}", "{startedAtTime}", "{recordedAt}"];

pub fn has_unique_token(format: Option<&str>) -> bool {
	format.is_some_and(|f| !f.trim().is_empty() && UNIQUE_TOKENS.iter().any(|t| f.to_lowercase().contains(&t.to_lowercase())))
}

/// Resolves a format to a file name (without extension). Times are local;
/// unknown tokens are left as written so a typo is visible in the name.
/// Invalid file name characters are removed.
pub fn build(
	format: Option<&str>,
	stream_id: Option<u64>,
	username: &str,
	started_at: Option<DateTime<Utc>>,
	recorded_at: DateTime<Local>,
	text: Option<&str>,
) -> String {
	let effective = match format.map(str::trim).filter(|f| !f.is_empty()) {
		Some(f) => f.to_string(),
		None => DEFAULT_FORMAT.to_string(),
	};
	let token_re = Regex::new(r"\{([^}]+)\}").unwrap();
	let name = token_re.replace_all(&effective, |caps: &regex::Captures| {
		let token = caps[1].to_lowercase();
		match token.as_str() {
			"id" => stream_id.map(|id| id.to_string()).unwrap_or_else(|| "unknown".to_string()),
			"username" => username.to_string(),
			"startedat" => started_at
				.unwrap_or_else(|| recorded_at.with_timezone(&Utc))
				.with_timezone(&Local)
				.format("%Y-%m-%d")
				.to_string(),
			"startedattime" => started_at
				.unwrap_or_else(|| recorded_at.with_timezone(&Utc))
				.with_timezone(&Local)
				.format("%Y-%m-%d_%H-%M-%S")
				.to_string(),
			"recordedat" => recorded_at.format("%Y-%m-%d_%H-%M-%S").to_string(),
			"text" => flatten_text(text),
			_ => format!("{{{token}}}"),
		}
	});
	let name = remove_invalid_file_name_chars(&name).trim().trim_end_matches('.').to_string();
	if name.is_empty() {
		remove_invalid_file_name_chars(username)
	} else {
		name
	}
}

fn flatten_text(text: Option<&str>) -> String {
	let flat = text.unwrap_or("")
		.split(['\r', '\n', '\t'])
		.map(str::trim)
		.filter(|part| !part.is_empty())
		.collect::<Vec<_>>()
		.join(" ");
	if flat.len() > TEXT_LENGTH_LIMIT { flat[..TEXT_LENGTH_LIMIT].trim().to_string() } else { flat }
}

pub fn remove_invalid_file_name_chars(name: &str) -> String {
	name.chars()
		.filter(|c| !matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*') && !c.is_control())
		.collect()
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn default_format_names_username_and_recorded_time() {
		let recorded = Local::now();
		let name = build(None, Some(5), "alice", None, recorded, None);
		assert!(name.starts_with("alice_"));
		assert!(!has_unique_token(None));
		assert!(has_unique_token(Some("{username}_{recordedAt}")));
	}

	#[test]
	fn tokens_are_substituted_and_invalid_chars_removed() {
		let recorded = Local::now();
		let name = build(Some("{id}-{username}-{text}"), Some(42), "bo/b", None, recorded, Some("hi\nthere"));
		assert_eq!(name, "42-bob-hi there");
	}
}
