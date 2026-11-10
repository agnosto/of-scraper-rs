//! Livestream recording: lists subscribed creators who are live and records a
//! creator's broadcast by joining the media vendor's web gateway as a
//! receive-only WebRTC viewer. Ported from SubScraper's feather-site
//! livestream support (`SubScraper.Core/Sites/Feather/Livestreams` +
//! `SubScraper.Core/Media`).
//!
//! Flow: `list_live` polls the signed subscriptions endpoint; `record` fetches
//! viewer credentials from `/users/{id}/streams/active/url`, discovers a media
//! gateway edge, joins over a gateway WebSocket, and receives the publisher's
//! VP8/H.264 + Opus RTP through a receive-only WebRTC peer. Media is written to
//! WebM/MKV as it arrives (no re-encode), then optionally converted with FFmpeg.

// Several items are kept for parity with the reference implementation's API
// surface (e.g. `LiveSession::disconnected`, `RtpReorderBuffer::drain`) even
// when the CLI path does not call them directly.
#![allow(dead_code)]

pub mod convert;
pub mod filenames;
mod mkv;
mod protocol;
mod recorder;
mod rtp;
mod session;
mod socket;
mod store;

use std::path::PathBuf;

use of_client::OFClient;

pub use recorder::{record, Progress, RecorderOptions};

/// The container a finished recording is converted to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OutputFormat {
	/// Keep the recorded file: WebM for VP8 video, Matroska for H.264. No FFmpeg needed.
	#[default]
	Original,
	/// Matroska. Copies the streams (no re-encode).
	Mkv,
	/// MP4 with H.264 video and AAC audio. VP8 video and Opus audio are re-encoded.
	Mp4,
}

impl OutputFormat {
	pub fn parse(value: &str) -> Self {
		match value.trim().to_lowercase().as_str() {
			"mkv" | "matroska" => Self::Mkv,
			"mp4" => Self::Mp4,
			_ => Self::Original,
		}
	}
}

/// Why a recording ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndReason {
	/// The stop was requested.
	Stopped,
	/// The creator was not broadcasting.
	NotLive,
	/// The broadcast ended.
	Ended,
}

/// Progress snapshot reported while recording.
#[derive(Debug, Clone)]
pub struct RecordingStatus {
	pub username: String,
	pub state: RecordingState,
	pub duration_ms: i64,
	pub bytes_written: u64,
	pub current_file: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordingState {
	Connecting,
	WaitingForMedia,
	Recording,
	Reconnecting,
	Converting,
	Finished,
}

/// A recording's outcome. Files are in `<creator>/Livestreams`, named from the
/// livestream filename format and converted to the configured container.
#[derive(Debug)]
pub struct RecordingResult {
	pub username: String,
	pub reason: EndReason,
	pub files: Vec<PathBuf>,
	pub duration_ms: i64,
	pub bytes: u64,
	pub warning: Option<String>,
}

/// Active subscriptions whose creators are broadcasting right now, as (username, user id).
pub async fn list_live(client: &OFClient, include_restricted: bool) -> of_client::reqwest_middleware::Result<Vec<(String, u64)>> {
	let live = client.get_live_subscriptions(include_restricted).await?;
	Ok(live.into_iter().map(|u| (u.username, u.id)).collect())
}
