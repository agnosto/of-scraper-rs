//! Livestream (Agora live broadcast) API: which subscribed creators are
//! live, the viewer credentials for joining a broadcast, and stream
//! metadata. All site API calls go through the signed `OFClient` middleware;
//! the media-vendor edge discovery is deliberately *not* signed and lives in
//! the caller, not here.

use chrono::{DateTime, Utc};
use log::warn;
use serde::Deserialize;

use crate::OFClient;

use super::user::User;

/// Viewer credentials for joining a creator's broadcast through the media
/// vendor's web gateway (`message.agora_cred` of the active-stream-url
/// response).
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct AgoraCredentials {
	pub app_id: String,
	pub channel: String,
	pub token: String,
	pub user_id: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ActiveStreamMessage {
	// The site sends `Success`/`Message` in PascalCase. Newtonsoft matched
	// those case-insensitively in SubScraper; serde does not, so accept both
	// spellings (and keep the lowercase one for older responses).
	#[serde(alias = "Success")]
	pub success: bool,
	#[serde(rename = "agora_cred")]
	pub agora_credentials: Option<AgoraCredentials>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ActiveStreamUrl {
	#[serde(alias = "Message")]
	pub message: Option<ActiveStreamMessage>,
}

/// A broadcast's metadata (`/users/{id}/streams/active` or `/streams/{id}`).
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StreamInfo {
	pub id: u64,
	pub description: Option<String>,
	#[serde(rename = "rawDescription")]
	pub raw_description: Option<String>,
	#[serde(default)]
	pub is_active: bool,
	#[serde(default)]
	pub is_finished: bool,
	pub started_at: Option<DateTime<Utc>>,
	pub finished_at: Option<DateTime<Utc>>,
	pub duration: Option<i64>,
	pub price: Option<f64>,
	pub room: Option<String>,
	pub views_count: Option<u64>,
	pub likes_count: Option<u64>,
	pub comments_count: Option<u64>,
}

/// The `extension` query value of the active-stream-url endpoint, URL-encoded
/// exactly as it must be sent: the request signature covers the query string
/// verbatim, so it is appended to the URL pre-encoded instead of going
/// through a query builder that would re-encode it. Matches the current web
/// player's viewer request (`{streamtype, sbp}`).
const STREAM_URL_EXTENSION_ENCODED: &str =
	"%7B%22streamtype%22%3A%22agora_direct%22%2C%22sbp%22%3Atrue%7D";

/// A stream is joinable only when the site returned a complete credential
/// set. Treating a half-empty one as valid only produces a confusing gateway
/// failure later.
fn usable_credentials(credentials: AgoraCredentials) -> Option<AgoraCredentials> {
	let complete = !credentials.app_id.trim().is_empty()
		&& !credentials.channel.trim().is_empty()
		&& !credentials.token.trim().is_empty();
	complete.then_some(credentials)
}

/// The site can decline a credentials request with a normal 200 response, so
/// this is logged (with secrets hidden) when recording cannot start.
fn sanitized_body(body: &str) -> String {
	let mut value: serde_json::Value = match serde_json::from_str(body) {
		Ok(value) => value,
		Err(_) => return format!("<non-JSON body, {} bytes>", body.len()),
	};
	redact(&mut value);
	let text = value.to_string();
	if text.chars().count() > 2_000 {
		text.chars().take(2_000).collect::<String>() + "…"
	} else {
		text
	}
}

fn redact(value: &mut serde_json::Value) {
	match value {
		serde_json::Value::Object(map) => {
			for (key, value) in map.iter_mut() {
				let key = key.to_ascii_lowercase();
				if ["token", "key", "secret", "cert", "cookie", "auth", "signature", "channel", "streamname"]
					.iter()
					.any(|needle| key.contains(needle))
				{
					*value = serde_json::Value::String("<redacted>".to_string());
				} else {
					redact(value);
				}
			}
		}
		serde_json::Value::Array(items) => items.iter_mut().for_each(redact),
		_ => {}
	}
}

/// Status code of a middleware error, when the failure was a response.
fn error_status(err: &reqwest_middleware::Error) -> Option<reqwest::StatusCode> {
	match err {
		reqwest_middleware::Error::Reqwest(e) => e.status(),
		_ => None,
	}
}

impl OFClient {
	/// Active subscriptions whose creators are broadcasting right now.
	///
	/// Reuses the subscription listing (with its rate-limit handling); a
	/// list item with `hasStream` is live.
	pub async fn get_live_subscriptions(
		&self,
		include_restricted: bool,
	) -> reqwest_middleware::Result<Vec<User>> {
		let subs = self.get_subscriptions_filtered(false, include_restricted).await?;
		Ok(subs.into_iter().filter(|u| u.has_stream == Some(true)).collect())
	}

	/// Viewer credentials for a creator's current broadcast, or `None` when
	/// the creator is not live (the endpoint answers 404/400 then).
	pub async fn get_stream_credentials(
		&self,
		user_id: u64,
	) -> reqwest_middleware::Result<Option<AgoraCredentials>> {
		let url = format!(
			"https://onlyfans.com/api2/v2/users/{user_id}/streams/active/url?extension={STREAM_URL_EXTENSION_ENCODED}"
		);
		match self.get(url).send().await {
			Ok(response) => {
				let body = response.text().await?;
				let dto: ActiveStreamUrl = serde_json::from_str(&body).map_err(reqwest_middleware::Error::middleware)?;
				match dto.message {
					Some(message) if message.success => match message.agora_credentials.and_then(usable_credentials) {
						Some(credentials) => Ok(Some(credentials)),
						None => {
							warn!("Livestream viewer credentials were empty; the site answered: {}", sanitized_body(&body));
							Ok(None)
						}
					},
					Some(_) => {
						warn!("The site declined livestream viewer credentials: {}", sanitized_body(&body));
						Ok(None)
					}
					None => {
						warn!("The site returned no livestream message: {}", sanitized_body(&body));
						Ok(None)
					}
				}
			}
			Err(err) => match error_status(&err) {
				Some(reqwest::StatusCode::NOT_FOUND) | Some(reqwest::StatusCode::BAD_REQUEST) => Ok(None),
				_ => Err(err),
			},
		}
	}

	/// Metadata of a creator's active broadcast, if any.
	pub async fn get_active_stream_info(
		&self,
		user_id: u64,
	) -> reqwest_middleware::Result<Option<StreamInfo>> {
		let url = format!("https://onlyfans.com/api2/v2/users/{user_id}/streams/active");
		match self.get(url).send().await {
			Ok(response) => {
				let info: StreamInfo = response.json().await?;
				Ok((info.id > 0).then_some(info))
			}
			Err(err) => match error_status(&err) {
				Some(reqwest::StatusCode::NOT_FOUND) | Some(reqwest::StatusCode::BAD_REQUEST) => Ok(None),
				_ => Err(err),
			},
		}
	}

	/// Metadata of a finished (or active) broadcast by stream id.
	pub async fn get_stream_info(&self, stream_id: u64) -> reqwest_middleware::Result<Option<StreamInfo>> {
		let url = format!("https://onlyfans.com/api2/v2/streams/{stream_id}");
		match self.get(url).send().await {
			Ok(response) => {
				let info: StreamInfo = response.json().await?;
				Ok((info.id > 0).then_some(info))
			}
			Err(err) => match error_status(&err) {
				Some(reqwest::StatusCode::NOT_FOUND) | Some(reqwest::StatusCode::BAD_REQUEST) => Ok(None),
				_ => Err(err),
			},
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn active_stream_url_parses_agora_cred() {
		let json = r#"{"message":{"success":true,"agora_cred":{"app_id":"abc","channel":"chan","token":"tok","user_id":42}}}"#;
		let dto: ActiveStreamUrl = serde_json::from_str(json).unwrap();
		let cred = dto.message.unwrap().agora_credentials.unwrap();
		assert_eq!(cred.app_id, "abc");
		assert_eq!(cred.channel, "chan");
		assert_eq!(cred.token, "tok");
		assert_eq!(cred.user_id, 42);
	}

	#[test]
	fn active_stream_url_accepts_the_pascal_case_response() {
		// Shape used by the current web player: top-level Token/Message,
		// capitalised Success, lower-case agora_cred.
		let json = r#"{
			"Token":"aaaa",
			"Message":{
				"MsgType":"agora_direct","Success":true,
				"agora_cred":{"app_id":"abc","channel":"chan","streamname":"stream","token":"tok","user_id":42},
				"sbp":{"endpoint":"https://example","token":"s","sbp_session":"x"}
			}
		}"#;
		let dto: ActiveStreamUrl = serde_json::from_str(json).unwrap();
		let message = dto.message.expect("Message must be recognised");
		assert!(message.success);
		let cred = message.agora_credentials.and_then(usable_credentials).expect("credentials");
		assert_eq!(cred.app_id, "abc");
		assert_eq!(cred.channel, "chan");
		assert_eq!(cred.token, "tok");
		assert_eq!(cred.user_id, 42);
	}

	#[test]
	fn active_stream_url_still_accepts_the_lower_case_response() {
		let json = r#"{"message":{"success":true,"agora_cred":{"app_id":"abc","channel":"chan","token":"tok","user_id":42}}}"#;
		let dto: ActiveStreamUrl = serde_json::from_str(json).unwrap();
		assert!(dto.message.unwrap().success);
	}

	#[test]
	fn incomplete_credentials_are_rejected() {
		let json = r#"{"message":{"success":true,"agora_cred":{"app_id":"abc","channel":"chan","token":"  ","user_id":42}}}"#;
		let dto: ActiveStreamUrl = serde_json::from_str(json).unwrap();
		assert!(dto.message.unwrap().agora_credentials.and_then(usable_credentials).is_none());
	}

	#[test]
	fn complete_credentials_are_accepted() {
		let json = r#"{"message":{"success":true,"agora_cred":{"app_id":"abc","channel":"chan","token":"tok","user_id":42}}}"#;
		let dto: ActiveStreamUrl = serde_json::from_str(json).unwrap();
		assert!(dto.message.unwrap().agora_credentials.and_then(usable_credentials).is_some());
	}

	#[test]
	fn extension_param_matches_the_site_expectation() {
		// {"streamtype":"agora_direct","sbp":true} percent-encoded.
		let decoded = urlencoding_decode(STREAM_URL_EXTENSION_ENCODED);
		assert_eq!(decoded, r#"{"streamtype":"agora_direct","sbp":true}"#);
	}

	#[test]
	fn diagnostics_keep_the_reason_but_hide_secrets() {
		let body = r#"{"message":{"success":false,"reason":"purchase_required","agora_cred":{"token":"secret-token","app_id":"app"}}}"#;
		let text = sanitized_body(body);
		assert!(text.contains("purchase_required"));
		assert!(text.contains(r#""app_id":"app""#));
		assert!(!text.contains("secret-token"));
	}

	#[test]
	fn credentials_url_signs_the_extension_exactly_as_sent() {
		// The middleware signs `path?query` from the parsed URL, so the parser
		// must hand back the extension bytes unchanged.
		let url = reqwest::Url::parse(&format!(
			"https://onlyfans.com/api2/v2/users/1/streams/active/url?extension={STREAM_URL_EXTENSION_ENCODED}"
		))
		.unwrap();
		assert_eq!(url.path(), "/api2/v2/users/1/streams/active/url");
		let expected = format!("extension={STREAM_URL_EXTENSION_ENCODED}");
		assert_eq!(url.query(), Some(expected.as_str()));
	}

	fn urlencoding_decode(s: &str) -> String {
		let bytes = s.as_bytes();
		let mut out = Vec::with_capacity(bytes.len());
		let mut i = 0;
		while i < bytes.len() {
			if bytes[i] == b'%' && i + 2 < bytes.len() {
				let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap();
				out.push(u8::from_str_radix(hex, 16).unwrap());
				i += 3;
			} else {
				out.push(bytes[i]);
				i += 1;
			}
		}
		String::from_utf8(out).unwrap()
	}
}
