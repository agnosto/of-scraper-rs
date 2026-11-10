//! Records one creator's broadcast, rejoining when the media connection
//! drops and finishing when the broadcaster has left and the site no longer
//! reports the stream as active. Files are named from the livestream filename
//! format, converted to the configured container at the end, and recorded with
//! the stream's metadata in the creator database. Ported from SubScraper's
//! `LivestreamRecorder`.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use chrono::{DateTime, Local, Utc};
use log::{info, warn};
use of_client::livestream::{AgoraCredentials, StreamInfo};
use of_client::OFClient;
use tokio_util::sync::CancellationToken;

use super::convert;
use super::filenames;
use super::mkv::LiveMuxer;
use super::rtp::{H264Depacketizer, LiveVideoCodec, RtpPacketInfo, RtpReorderBuffer, VideoDepacketizer, Vp8Depacketizer};
use super::session::{LiveSession, MediaKind};
use super::store::{self, LivestreamRecord, RecordingRecord};
use super::{EndReason, OutputFormat, RecordingResult, RecordingState, RecordingStatus};

/// Per-recording settings taken from the configuration when recording starts.
#[derive(Debug, Clone)]
pub struct RecorderOptions {
	pub file_name_format: Option<String>,
	pub output_format: OutputFormat,
	pub ffmpeg_path: Option<PathBuf>,
}

pub type Progress = Box<dyn Fn(RecordingStatus) + Send + Sync>;

const TICK: std::time::Duration = std::time::Duration::from_millis(250);
const PROGRESS_INTERVAL_MS: i64 = 1000;
const BROADCASTER_GRACE_PERIOD_MS: i64 = 15_000;
const MEDIA_STALL_TIMEOUT_MS: i64 = 20_000;
const FIRST_MEDIA_TIMEOUT_MS: i64 = 30_000;
const KEY_FRAME_REQUEST_INTERVAL_MS: i64 = 1000;
const MAX_CONSECUTIVE_FAILURES: u32 = 5;
const MAX_SESSIONS_WITHOUT_MEDIA: u32 = 3;
const LIVESTREAMS_FOLDER: &str = "Livestreams";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SessionOutcome {
	Disconnected,
	BroadcasterGone,
	Stopped,
}

/// One file of the recording, filled in as it is written and finished.
struct RecordedFile {
	part: u32,
	path: PathBuf,
	stream: Option<StreamInfo>,
	started_at: DateTime<Utc>,
	finished_at: DateTime<Utc>,
	duration_ms: i64,
}

/// Records `username`'s current or next broadcast into `<creator_path>/Livestreams`.
#[allow(clippy::too_many_arguments)]
pub async fn record(
	client: &OFClient,
	username: &str,
	user_id: u64,
	creator_path: &Path,
	options: &RecorderOptions,
	user_agent: &str,
	http: &reqwest::Client,
	progress: Option<Progress>,
	ct: &CancellationToken,
) -> anyhow::Result<RecordingResult> {
	let directory = creator_path.join(LIVESTREAMS_FOLDER);
	let clock = Instant::now();
	let now_ms = || clock.elapsed().as_millis() as i64;
	let files: Arc<Mutex<Vec<RecordedFile>>> = Arc::new(Mutex::new(Vec::new()));
	let current_stream: Arc<Mutex<Option<StreamInfo>>> = Arc::new(Mutex::new(None));
	let mut stream: Option<StreamInfo> = None;

	let files_for_muxer = files.clone();
	let stream_for_muxer = current_stream.clone();
	let format_for_muxer = options.file_name_format.clone();
	let username_for_muxer = username.to_string();
	let mut muxer = LiveMuxer::new(
		move |part, extension| {
			let now = Local::now();
			let stream = stream_for_muxer.lock().unwrap().clone();
			let name = filenames::build(
				format_for_muxer.as_deref(),
				stream.as_ref().map(|s| s.id),
				&username_for_muxer,
				stream.as_ref().and_then(|s| s.started_at),
				now,
				stream.as_ref().and_then(|s| s.raw_description.as_deref().or(s.description.as_deref())),
			);
			std::fs::create_dir_all(&directory)?;
			let path = unique_path(&directory, &name, extension);
			files_for_muxer.lock().unwrap().push(RecordedFile {
				part,
				path: path.clone(),
				stream,
				started_at: now.with_timezone(&Utc),
				finished_at: now.with_timezone(&Utc),
				duration_ms: 0,
			});
			info!("Recording {username_for_muxer} to {}", path.display());
			std::fs::File::create(&path)
		},
		2,
	);
	{
		let files = files.clone();
		muxer.part_completed = Some(Box::new(move |part| {
			if let Some(file) = files.lock().unwrap().iter_mut().find(|f| f.part == part.number) {
				file.finished_at = Utc::now();
				file.duration_ms = part.duration_ms;
			}
		}));
	}

	let report = |state: RecordingState, duration_ms: i64, bytes: u64| {
		if let Some(progress) = &progress {
			let current_file = files.lock().unwrap().last().map(|f| f.path.clone());
			progress(RecordingStatus { username: username.to_string(), state, duration_ms, bytes_written: bytes, current_file });
		}
	};
	macro_rules! report_stats {
		($state:expr) => {
			report($state, muxer.duration_ms().unwrap_or(0), muxer.bytes_written().unwrap_or(0))
		};
	}

	let mut reason = EndReason::Stopped;
	let mut failures = 0u32;
	let mut sessions_without_media = 0u32;
	report_stats!(RecordingState::Connecting);
	loop {
		if ct.is_cancelled() {
			reason = EndReason::Stopped;
			break;
		}

		let credentials = match client.get_stream_credentials(user_id).await {
			Ok(Some(credentials)) => credentials,
			Ok(None) => {
				reason = if muxer.is_started() { EndReason::Ended } else { EndReason::NotLive };
				break;
			}
			Err(e) => {
				failures += 1;
				if failures >= MAX_CONSECUTIVE_FAILURES {
					warn!("Livestream credentials request for {username} failed {failures} times: {e}");
					reason = if muxer.is_started() { EndReason::Ended } else { EndReason::NotLive };
					break;
				}
				warn!("Livestream credentials request for {username} failed (attempt {failures}): {e}");
				if !sleep_or_stop(2 * failures as u64, ct, &mut reason).await {
					break;
				}
				report_stats!(RecordingState::Reconnecting);
				continue;
			}
		};

		if let Some(info) = try_get_stream_info(client, user_id).await {
			let mut current = current_stream.lock().unwrap();
			if let Some(previous) = &*current
				&& previous.id != info.id
			{
				// The creator started a new stream: keep it in its own file, named for it.
				info!("{username} started a new stream ({})", info.id);
				let _ = muxer.begin_new_part();
			}
			*current = Some(info.clone());
			drop(current);
			stream = Some(info.clone());
			store::upsert_livestream(creator_path, &to_record(&info));
		}

		match run_session(&credentials, user_agent, http, &mut muxer, now_ms, &report, ct).await {
			Ok((SessionOutcome::Stopped, _)) => {
				reason = EndReason::Stopped;
				break;
			}
			Ok((outcome, received_media)) => {
				failures = 0;
				sessions_without_media = if received_media { 0 } else { sessions_without_media + 1 };
				if sessions_without_media >= MAX_SESSIONS_WITHOUT_MEDIA {
					reason = if muxer.is_started() { EndReason::Ended } else { EndReason::NotLive };
					break;
				}
				info!("Broadcast session for {username} ended ({outcome:?}); checking whether it continues");
			}
			Err(e) => {
				failures += 1;
				if failures >= MAX_CONSECUTIVE_FAILURES {
					warn!("Broadcast connection for {username} failed {failures} times: {e}");
					reason = if muxer.is_started() { EndReason::Ended } else { EndReason::NotLive };
					break;
				}
				warn!("Broadcast connection for {username} failed (attempt {failures}): {e}");
				if !sleep_or_stop(2 * failures as u64, ct, &mut reason).await {
					break;
				}
			}
		}

		report_stats!(RecordingState::Reconnecting);
	}

	let _ = muxer.finish();
	let duration_ms = muxer.duration_ms().unwrap_or(0);
	let bytes = muxer.bytes_written().unwrap_or(0);
	let warning = finish_files(client, creator_path, &files, stream.as_ref(), reason, options, &report).await;
	report_stats!(RecordingState::Finished);

	let paths: Vec<PathBuf> = files.lock().unwrap().iter().map(|f| f.path.clone()).collect();
	let total_bytes = if paths.is_empty() { bytes } else { paths.iter().map(|p| std::fs::metadata(p).map(|m| m.len()).unwrap_or(0)).sum() };
	Ok(RecordingResult {
		username: username.to_string(),
		reason,
		files: paths,
		duration_ms,
		bytes: total_bytes,
		warning,
	})
}

/// Returns false when the stop was requested during the backoff.
async fn sleep_or_stop(seconds: u64, ct: &CancellationToken, reason: &mut EndReason) -> bool {
	tokio::select! {
		_ = ct.cancelled() => { *reason = EndReason::Stopped; false }
		_ = tokio::time::sleep(std::time::Duration::from_secs(seconds)) => true,
	}
}

async fn try_get_stream_info(client: &OFClient, user_id: u64) -> Option<StreamInfo> {
	match client.get_active_stream_info(user_id).await {
		Ok(info) => info,
		Err(e) => {
			// Metadata improves names and the database, but a recording must not fail without it.
			warn!("Could not read livestream details for user {user_id}: {e}");
			None
		}
	}
}

fn to_record(info: &StreamInfo) -> LivestreamRecord {
	let text = info
		.raw_description
		.clone()
		.filter(|t| !t.trim().is_empty())
		.unwrap_or_else(|| info.description.clone().unwrap_or_default().trim().to_string());
	let duration = if info.is_finished {
		info.duration.filter(|d| *d > 0).or_else(|| match (info.started_at, info.finished_at) {
			(Some(started), Some(finished)) if finished > started => Some((finished - started).num_seconds()),
			_ => None,
		})
	} else {
		None
	};
	LivestreamRecord {
		stream_id: info.id,
		text: text.clone(),
		raw_text: info.description.clone().unwrap_or_default(),
		price: info.price.unwrap_or(0.0) as i64,
		started_at: info.started_at,
		finished_at: info.finished_at,
		duration_secs: duration,
		views_count: info.views_count.unwrap_or(0),
		likes_count: info.likes_count.unwrap_or(0),
		comments_count: info.comments_count.unwrap_or(0),
	}
}

/// Stores the streams' final metadata, converts the files, and records each one in the database.
async fn finish_files(
	client: &OFClient,
	creator_path: &Path,
	files: &Arc<Mutex<Vec<RecordedFile>>>,
	stream: Option<&StreamInfo>,
	reason: EndReason,
	options: &RecorderOptions,
	report: &(dyn Fn(RecordingState, i64, u64) + Send + Sync),
) -> Option<String> {
	// Once a stream has ended, its own endpoint reports when it finished and how long it ran.
	let mut seen = std::collections::HashSet::new();
	let mut infos: Vec<StreamInfo> = files
		.lock()
		.unwrap()
		.iter()
		.filter_map(|f| f.stream.clone())
		.chain(stream.cloned())
		.filter(|info| seen.insert(info.id))
		.collect();
	for recorded in &mut infos {
		if let Ok(Some(final_info)) = client.get_stream_info(recorded.id).await {
			*recorded = final_info;
		}
		store::upsert_livestream(creator_path, &to_record(recorded));
	}

	let mut warning = None;
	if options.output_format != OutputFormat::Original && !files.lock().unwrap().is_empty() {
		report(RecordingState::Converting, 0, 0);
		let paths: Vec<PathBuf> = files.lock().unwrap().iter().map(|f| f.path.clone()).collect();
		for path in paths {
			// Converting is part of finishing a recording, so a stop request does not abandon it.
			let conversion = convert::convert(&path, options.output_format, options.ffmpeg_path.as_deref()).await;
			warning = warning.or(conversion.error);
			let mut files = files.lock().unwrap();
			if let Some(file) = files.iter_mut().find(|f| f.path == path) {
				file.path = conversion.path;
			}
		}
	}

	for file in files.lock().unwrap().iter() {
		let extension = file.path.extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();
		store::add_recording(
			creator_path,
			&RecordingRecord {
				stream_id: file.stream.as_ref().map(|s| s.id).or(stream.map(|s| s.id)).unwrap_or(0),
				part: file.part,
				directory: file.path.parent().map(|p| p.to_string_lossy().to_string()).unwrap_or_default(),
				filename: file.path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default(),
				size: std::fs::metadata(&file.path).map(|m| m.len()).unwrap_or(0),
				duration_ms: file.duration_ms,
				recording_started_at: file.started_at,
				recording_finished_at: file.finished_at,
				end_reason: format!("{reason:?}"),
				format: extension,
			},
		);
	}

	warning
}

/// One broadcast session: connects, then pumps media until it ends.
async fn run_session(
	credentials: &AgoraCredentials,
	user_agent: &str,
	http: &reqwest::Client,
	muxer: &mut LiveMuxer,
	now_ms: impl Fn() -> i64,
	report: &(dyn Fn(RecordingState, i64, u64) + Send + Sync),
	ct: &CancellationToken,
) -> anyhow::Result<(SessionOutcome, bool)> {
	let session = LiveSession::connect(credentials, user_agent, http)
		.await
		.map_err(|e| anyhow::anyhow!("{e}"))?;
	let LiveSession {
		peer,
		socket,
		mut video_rx,
		mut audio_rx,
		mut presence_rx,
		disconnected_rx,
	} = session;
	let mut pipeline = MediaPipeline::new();
	let mut broadcaster_left_at_ms: i64 = -1;
	let connected_at_ms = now_ms();
	let mut last_progress_ms = 0i64;
	report(RecordingState::WaitingForMedia, 0, 0);

	let mut tick = tokio::time::interval(TICK);
	tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
	let mut disconnected_rx = disconnected_rx;

	let outcome = loop {
		tokio::select! {
			_ = ct.cancelled() => break SessionOutcome::Stopped,
			_ = tick.tick() => {
				let now = now_ms();
				pipeline.tick(now, &peer, muxer).await?;
				let recording = muxer.is_writing();
				if now - last_progress_ms >= PROGRESS_INTERVAL_MS {
					last_progress_ms = now;
					report(
						if recording { RecordingState::Recording } else { RecordingState::WaitingForMedia },
						muxer.duration_ms().unwrap_or(0),
						muxer.bytes_written().unwrap_or(0),
					);
				}
				if let Some(reason) = disconnected_rx.borrow().clone() {
					info!("Broadcast media connection lost: {reason}");
					break SessionOutcome::Disconnected;
				}
				if broadcaster_left_at_ms >= 0 && now - broadcaster_left_at_ms >= BROADCASTER_GRACE_PERIOD_MS {
					break SessionOutcome::BroadcasterGone;
				}
				let last_media = pipeline.last_media_ms;
				if last_media >= 0 && now - last_media >= MEDIA_STALL_TIMEOUT_MS
					|| last_media < 0 && now - connected_at_ms >= FIRST_MEDIA_TIMEOUT_MS
				{
					info!("No broadcast media received for a while; reconnecting");
					break SessionOutcome::Disconnected;
				}
			}
			Some((packet, codec)) = video_rx.recv() => {
				let now = now_ms();
				pipeline.on_video_packet(packet, codec, &peer, muxer, now).await?;
			}
			Some(packet) = audio_rx.recv() => {
				let now = now_ms();
				pipeline.on_audio_packet(packet, &peer, muxer, now).await?;
			}
			Some(left) = presence_rx.recv() => {
				broadcaster_left_at_ms = if left { now_ms() } else { -1 };
			}
			changed = disconnected_rx.changed() => {
				if changed.is_ok() {
					if let Some(reason) = disconnected_rx.borrow().clone() {
						info!("Broadcast media connection lost: {reason}");
						break SessionOutcome::Disconnected;
					}
				} else {
					break SessionOutcome::Disconnected;
				}
			}
		}
	};

	let received_media = pipeline.received_media();
	LiveSession::dispose_parts(peer, socket).await;
	Ok((outcome, received_media))
}

/// Per-session packet handling: reordering, retransmission requests, depacketizing and muxing.
struct MediaPipeline {
	video_buffer: Option<RtpReorderBuffer>,
	audio_buffer: Option<RtpReorderBuffer>,
	depacketizer: Option<Box<dyn VideoDepacketizer + Send>>,
	video_ssrc: u32,
	audio_ssrc: u32,
	need_key_frame: bool,
	last_key_frame_request_ms: i64,
	last_media_ms: i64,
}

impl MediaPipeline {
	fn new() -> Self {
		Self {
			video_buffer: None,
			audio_buffer: None,
			depacketizer: None,
			video_ssrc: 0,
			audio_ssrc: 0,
			need_key_frame: true,
			last_key_frame_request_ms: -KEY_FRAME_REQUEST_INTERVAL_MS,
			last_media_ms: -1,
		}
	}

	fn received_media(&self) -> bool {
		self.last_media_ms >= 0
	}

	async fn on_video_packet(
		&mut self,
		packet: RtpPacketInfo,
		codec: LiveVideoCodec,
		peer: &Arc<webrtc::peer_connection::RTCPeerConnection>,
		muxer: &mut LiveMuxer,
		now: i64,
	) -> anyhow::Result<()> {
		self.last_media_ms = now;
		let ssrc = packet.ssrc;
		if self.video_buffer.is_none()
			|| ssrc != self.video_ssrc
			|| self.depacketizer.as_ref().map(|d| d.codec()) != Some(codec)
		{
			self.video_ssrc = ssrc;
			self.depacketizer = Some(match codec {
				LiveVideoCodec::H264 => Box::new(H264Depacketizer::new()),
				LiveVideoCodec::Vp8 => Box::new(Vp8Depacketizer::new()),
			});
			self.video_buffer = Some(RtpReorderBuffer::new());
			self.need_key_frame = true;
			self.request_key_frame(peer, now).await;
		}

		let (released, missing) = self.video_buffer.as_mut().unwrap().push(packet, now);
		if !missing.is_empty() {
			LiveSession::request_retransmission_on(peer, MediaKind::Video, ssrc, &missing).await;
		}
		for packet in released {
			self.on_ordered_video(packet, peer, muxer, now).await?;
		}
		Ok(())
	}

	async fn on_audio_packet(
		&mut self,
		packet: RtpPacketInfo,
		peer: &Arc<webrtc::peer_connection::RTCPeerConnection>,
		muxer: &mut LiveMuxer,
		now: i64,
	) -> anyhow::Result<()> {
		self.last_media_ms = now;
		let ssrc = packet.ssrc;
		if self.audio_buffer.is_none() || ssrc != self.audio_ssrc {
			self.audio_ssrc = ssrc;
			self.audio_buffer = Some(RtpReorderBuffer::new());
		}
		let (released, missing) = self.audio_buffer.as_mut().unwrap().push(packet, now);
		if !missing.is_empty() {
			LiveSession::request_retransmission_on(peer, MediaKind::Audio, ssrc, &missing).await;
		}
		for packet in released {
			muxer.add_audio_frame(packet.payload, packet.timestamp, ssrc, now)?;
		}
		Ok(())
	}

	async fn tick(
		&mut self,
		now: i64,
		peer: &Arc<webrtc::peer_connection::RTCPeerConnection>,
		muxer: &mut LiveMuxer,
	) -> anyhow::Result<()> {
		if let Some(buffer) = &mut self.video_buffer {
			let (released, losses) = buffer.flush(now);
			for packet in released {
				self.on_ordered_video(packet, peer, muxer, now).await?;
			}
			for _ in losses {
				self.on_video_loss(peer, now).await;
			}
		}
		if let Some(buffer) = &mut self.audio_buffer {
			let (released, _) = buffer.flush(now);
			for packet in released {
				let ssrc = self.audio_ssrc;
				muxer.add_audio_frame(packet.payload, packet.timestamp, ssrc, now)?;
			}
		}
		if self.need_key_frame && self.video_buffer.is_some() {
			self.request_key_frame(peer, now).await;
		}
		muxer.pump(now)?;
		Ok(())
	}

	async fn on_ordered_video(
		&mut self,
		packet: RtpPacketInfo,
		peer: &Arc<webrtc::peer_connection::RTCPeerConnection>,
		muxer: &mut LiveMuxer,
		now: i64,
	) -> anyhow::Result<()> {
		let Some(depacketizer) = &mut self.depacketizer else { return Ok(()) };
		let Some(frame) = depacketizer.push(&packet) else { return Ok(()) };
		if self.need_key_frame && !frame.is_key_frame {
			return Ok(());
		}
		let codec = depacketizer.codec();
		let codec_private = depacketizer.decoder_configuration();
		if codec == LiveVideoCodec::H264 && codec_private.is_none() {
			// An IDR without parameter sets cannot start a decodable track; ask for a key frame that has them.
			self.need_key_frame = true;
			self.request_key_frame(peer, now).await;
			return Ok(());
		}
		self.need_key_frame = false;
		muxer.add_video_frame(frame, codec, codec_private.as_deref(), self.video_ssrc, now)?;
		Ok(())
	}

	async fn on_video_loss(&mut self, peer: &Arc<webrtc::peer_connection::RTCPeerConnection>, now: i64) {
		// Frames after a lost packet reference a damaged picture, so wait for the next key frame.
		if let Some(depacketizer) = &mut self.depacketizer {
			depacketizer.reset();
		}
		self.need_key_frame = true;
		self.request_key_frame(peer, now).await;
	}

	async fn request_key_frame(&mut self, peer: &Arc<webrtc::peer_connection::RTCPeerConnection>, now: i64) {
		if now - self.last_key_frame_request_ms < KEY_FRAME_REQUEST_INTERVAL_MS {
			return;
		}
		self.last_key_frame_request_ms = now;
		LiveSession::request_key_frame_on(peer, self.video_ssrc).await;
	}
}

fn unique_path(directory: &Path, base_name: &str, extension: &str) -> PathBuf {
	let mut path = directory.join(format!("{base_name}{extension}"));
	let mut suffix = 2;
	while path.exists() {
		path = directory.join(format!("{base_name}_{suffix}{extension}"));
		suffix += 1;
	}
	path
}
