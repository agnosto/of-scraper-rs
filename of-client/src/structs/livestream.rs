//! Livestream (Agora live broadcast) API: which subscribed creators are
//! live, the viewer credentials for joining a broadcast, and stream
//! metadata. All site API calls go through the signed `OFClient` middleware;
//! the media-vendor edge discovery is deliberately *not* signed and lives in
//! the caller, not here.

use chrono::{DateTime, Utc};
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
	pub success: bool,
	#[serde(rename = "agora_cred")]
	pub agora_credentials: Option<AgoraCredentials>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ActiveStreamUrl {
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
/// through a query builder that would re-encode it.
const STREAM_URL_EXTENSION_ENCODED: &str =
	"%7B%22streamtype%22%3A%22agora_direct%22%2C%22sbp%22%3Atrue%2C%22preferred_servers%22%3A%5B%5D%7D";

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
				let dto: ActiveStreamUrl = response.json().await?;
				Ok(match dto.message {
					Some(m) if m.success => m.agora_credentials,
					_ => None,
				})
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
	fn extension_param_matches_the_site_expectation() {
		// {"streamtype":"agora_direct","sbp":true,"preferred_servers":[]} percent-encoded.
		let decoded = urlencoding_decode(STREAM_URL_EXTENSION_ENCODED);
		assert_eq!(decoded, r#"{"streamtype":"agora_direct","sbp":true,"preferred_servers":[]}"#);
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
