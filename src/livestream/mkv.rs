//! Matroska/WebM writing while media arrives: a minimal EBML encoder, a live
//! writer that closes each cluster as soon as it fills (so an interrupted
//! recording is still playable), and a muxer that turns encoded video plus
//! Opus audio into part files. Ported from `SubScraper.Core/Media/Matroska`
//! and `LiveRecordingMuxer`.

use std::io::{self, Seek, SeekFrom, Write};

use chrono::{DateTime, Utc};

use super::rtp::{EncodedVideoFrame, LiveVideoCodec, RtpTimeline};

// EBML element ids.
const EBML_HEADER: u32 = 0x1A45DFA3;
const EBML_VERSION: u32 = 0x4286;
const EBML_READ_VERSION: u32 = 0x42F7;
const EBML_MAX_ID_LENGTH: u32 = 0x42F2;
const EBML_MAX_SIZE_LENGTH: u32 = 0x42F3;
const DOC_TYPE: u32 = 0x4282;
const DOC_TYPE_VERSION: u32 = 0x4287;
const DOC_TYPE_READ_VERSION: u32 = 0x4285;
const SEGMENT: u32 = 0x18538067;
const SEEK_HEAD: u32 = 0x114D9B74;
const SEEK: u32 = 0x4DBB;
const SEEK_ID: u32 = 0x53AB;
const SEEK_POSITION: u32 = 0x53AC;
const VOID: u32 = 0xEC;
const INFO: u32 = 0x1549A966;
const TIMESTAMP_SCALE: u32 = 0x2AD7B1;
const MUXING_APP: u32 = 0x4D80;
const WRITING_APP: u32 = 0x5741;
const DATE_UTC: u32 = 0x4461;
const DURATION: u32 = 0x4489;
const TRACKS: u32 = 0x1654AE6B;
const TRACK_ENTRY: u32 = 0xAE;
const TRACK_NUMBER: u32 = 0xD7;
const TRACK_UID: u32 = 0x73C5;
const TRACK_TYPE: u32 = 0x83;
const FLAG_LACING: u32 = 0x9C;
const CODEC_ID: u32 = 0x86;
const CODEC_PRIVATE: u32 = 0x63A2;
const CODEC_DELAY: u32 = 0x56AA;
const SEEK_PRE_ROLL: u32 = 0x56BB;
const VIDEO: u32 = 0xE0;
const PIXEL_WIDTH: u32 = 0xB0;
const PIXEL_HEIGHT: u32 = 0xBA;
const AUDIO: u32 = 0xE1;
const SAMPLING_FREQUENCY: u32 = 0xB5;
const CHANNELS: u32 = 0x9F;
const CLUSTER: u32 = 0x1F43B675;
const TIMESTAMP: u32 = 0xE7;
const SIMPLE_BLOCK: u32 = 0xA3;
const CUES: u32 = 0x1C53BB6B;
const CUE_POINT: u32 = 0xBB;
const CUE_TIME: u32 = 0xB3;
const CUE_TRACK_POSITIONS: u32 = 0xB7;
const CUE_TRACK: u32 = 0xF7;
const CUE_CLUSTER_POSITION: u32 = 0xF1;

/// An 8-byte size field meaning "unknown"; also reserves room to patch the real size later.
const UNKNOWN_SIZE: [u8; 8] = [0x01, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF];

fn write_id<W: Write>(w: &mut W, id: u32) -> io::Result<()> {
	let length = if id > 0xFF_FFFF { 4 } else if id > 0xFFFF { 3 } else if id > 0xFF { 2 } else { 1 };
	for i in (0..length).rev() {
		w.write_all(&[(id >> (8 * i)) as u8])?;
	}
	Ok(())
}

fn write_size<W: Write>(w: &mut W, size: i64) -> io::Result<()> {
	let mut length = 1;
	while length < 8 && size >= (1i64 << (7 * length)) - 1 {
		length += 1;
	}
	write_size_exact(w, size, length)
}

/// Writes a size using exactly `length` bytes so it can be patched in place.
fn write_size_exact<W: Write>(w: &mut W, size: i64, length: usize) -> io::Result<()> {
	let value = size as u64 | (1u64 << (7 * length));
	for i in (0..length).rev() {
		w.write_all(&[(value >> (8 * i)) as u8])?;
	}
	Ok(())
}

fn write_unsigned<W: Write>(w: &mut W, id: u32, value: u64) -> io::Result<()> {
	let mut length = 1;
	while length < 8 && value >> (8 * length) != 0 {
		length += 1;
	}
	write_id(w, id)?;
	write_size(w, length as i64)?;
	for i in (0..length).rev() {
		w.write_all(&[(value >> (8 * i)) as u8])?;
	}
	Ok(())
}

fn write_float<W: Write>(w: &mut W, id: u32, value: f64) -> io::Result<()> {
	write_id(w, id)?;
	write_size(w, 8)?;
	w.write_all(&value.to_be_bytes())
}

fn write_string<W: Write>(w: &mut W, id: u32, value: &str) -> io::Result<()> {
	write_binary(w, id, value.as_bytes())
}

fn write_binary<W: Write>(w: &mut W, id: u32, value: &[u8]) -> io::Result<()> {
	write_id(w, id)?;
	write_size(w, value.len() as i64)?;
	w.write_all(value)
}

fn write_master<W: Write>(w: &mut W, id: u32, write_body: impl FnOnce(&mut Vec<u8>) -> io::Result<()>) -> io::Result<()> {
	let mut body = Vec::new();
	write_body(&mut body)?;
	write_id(w, id)?;
	write_size(w, body.len() as i64)?;
	w.write_all(&body)
}

/// Fills exactly `total_length` bytes (at least two) with a Void element.
fn write_void<W: Write>(w: &mut W, total_length: usize) -> io::Result<()> {
	write_id(w, VOID)?;
	let payload = total_length - 1 - 8;
	write_size_exact(w, payload as i64, 8)?;
	w.write_all(&vec![0u8; payload])
}

fn id_bytes(id: u32) -> Vec<u8> {
	let mut v = Vec::new();
	write_id(&mut v, id).unwrap();
	v
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MatroskaTrackType {
	Video = 1,
	Audio = 2,
}

#[derive(Debug, Clone)]
pub struct MatroskaTrack {
	pub number: u32,
	pub track_type: MatroskaTrackType,
	pub codec_id: &'static str,
	pub codec_private: Option<Vec<u8>>,
	pub pixel_width: u32,
	pub pixel_height: u32,
	pub sampling_frequency: f64,
	pub channels: u32,
	pub codec_delay_ns: u64,
	pub seek_pre_roll_ns: u64,
}

/// Writes a Matroska/WebM file while media arrives. Each cluster is written
/// as soon as it closes, so a recording interrupted at any point is still
/// playable; [`MatroskaLiveWriter::finish`] adds the duration, cues and seek
/// index. Timestamps are in milliseconds.
pub struct MatroskaLiveWriter<W: Write + Seek> {
	stream: W,
	has_video: bool,
	segment_size_position: u64,
	segment_data_start: u64,
	seek_head_position: u64,
	info_position: u64,
	tracks_position: u64,
	duration_value_position: u64,
	cues: Vec<(i64, u32, u64)>,
	cluster: Vec<u8>,
	cluster_start_ms: i64,
	finished: bool,
	last_timestamp_ms: i64,
}

const SEEK_HEAD_RESERVED_BYTES: usize = 96;
const MIN_CLUSTER_MS: i64 = 1000;
const MAX_CLUSTER_MS: i64 = 5000;

impl<W: Write + Seek> MatroskaLiveWriter<W> {
	pub fn new(stream: W, doc_type: &str, tracks: &[MatroskaTrack], created_utc: DateTime<Utc>) -> io::Result<Self> {
		let mut stream = stream;
		let has_video = tracks.iter().any(|t| t.track_type == MatroskaTrackType::Video);

		write_master(&mut stream, EBML_HEADER, |header| {
			write_unsigned(header, EBML_VERSION, 1)?;
			write_unsigned(header, EBML_READ_VERSION, 1)?;
			write_unsigned(header, EBML_MAX_ID_LENGTH, 4)?;
			write_unsigned(header, EBML_MAX_SIZE_LENGTH, 8)?;
			write_string(header, DOC_TYPE, doc_type)?;
			write_unsigned(header, DOC_TYPE_VERSION, 4)?;
			write_unsigned(header, DOC_TYPE_READ_VERSION, 2)
		})?;

		write_id(&mut stream, SEGMENT)?;
		let segment_size_position = stream.stream_position()?;
		stream.write_all(&UNKNOWN_SIZE)?;
		let segment_data_start = stream.stream_position()?;

		let seek_head_position = stream.stream_position()?;
		write_void(&mut stream, SEEK_HEAD_RESERVED_BYTES)?;

		let info_position = stream.stream_position()?;
		let mut info = Vec::new();
		write_unsigned(&mut info, TIMESTAMP_SCALE, 1_000_000)?;
		write_string(&mut info, MUXING_APP, "of-scraper-rs")?;
		write_string(&mut info, WRITING_APP, "of-scraper-rs")?;
		let nanoseconds_since_2001 = nanoseconds_since_2001(created_utc);
		write_id(&mut info, DATE_UTC)?;
		write_size(&mut info, 8)?;
		info.write_all(&nanoseconds_since_2001.to_be_bytes())?;
		let duration_offset = info.len();
		write_float(&mut info, DURATION, 0.0)?;

		write_id(&mut stream, INFO)?;
		write_size(&mut stream, info.len() as i64)?;
		let duration_value_position = stream.stream_position()? + duration_offset as u64 + 3; // after the 2-byte ID and size byte
		stream.write_all(&info)?;

		let tracks_position = stream.stream_position()?;
		write_master(&mut stream, TRACKS, |body| {
			for track in tracks {
				write_master(body, TRACK_ENTRY, |entry| write_track(entry, track))?;
			}
			Ok(())
		})?;
		stream.flush()?;

		Ok(Self {
			stream,
			has_video,
			segment_size_position,
			segment_data_start,
			seek_head_position,
			info_position,
			tracks_position,
			duration_value_position,
			cues: Vec::new(),
			cluster: Vec::new(),
			cluster_start_ms: -1,
			finished: false,
			last_timestamp_ms: 0,
		})
	}

	/// Bytes written to the file so far, excluding the open cluster.
	pub fn bytes_written(&mut self) -> io::Result<u64> {
		self.stream.stream_position()
	}

	pub fn last_timestamp_ms(&self) -> i64 {
		self.last_timestamp_ms
	}

	pub fn write_frame(&mut self, track: u32, timestamp_ms: i64, is_key_frame: bool, is_video: bool, data: &[u8]) -> io::Result<()> {
		if self.finished {
			return Err(io::Error::other("writer finished"));
		}
		let timestamp_ms = timestamp_ms.max(0);
		let since_cluster_start = timestamp_ms - self.cluster_start_ms;
		let start_cluster = self.cluster_start_ms < 0
			|| since_cluster_start >= MAX_CLUSTER_MS
			|| since_cluster_start < i16::MIN as i64
			|| (since_cluster_start >= MIN_CLUSTER_MS && if is_video { is_key_frame } else { !self.has_video });
		if start_cluster {
			self.flush_cluster()?;
			self.cluster_start_ms = timestamp_ms;
			write_unsigned(&mut self.cluster, TIMESTAMP, timestamp_ms as u64)?;
			if !self.has_video || (is_video && is_key_frame) {
				let cluster_position = self.stream.stream_position()? - self.segment_data_start;
				self.cues.push((timestamp_ms, track, cluster_position));
			}
		}

		write_id(&mut self.cluster, SIMPLE_BLOCK)?;
		write_size(&mut self.cluster, (4 + data.len()) as i64)?;
		self.cluster.push(0x80 | track as u8);
		self.cluster.extend_from_slice(&((timestamp_ms - self.cluster_start_ms) as i16).to_be_bytes());
		self.cluster.push(if is_key_frame { 0x80 } else { 0x00 });
		self.cluster.extend_from_slice(data);
		self.last_timestamp_ms = self.last_timestamp_ms.max(timestamp_ms);
		Ok(())
	}

	/// Writes the open cluster to the file and flushes it to disk.
	pub fn flush_cluster(&mut self) -> io::Result<()> {
		if self.cluster.is_empty() {
			return Ok(());
		}
		write_id(&mut self.stream, CLUSTER)?;
		write_size(&mut self.stream, self.cluster.len() as i64)?;
		self.stream.write_all(&self.cluster)?;
		self.cluster.clear();
		self.cluster_start_ms = -1;
		self.stream.flush()
	}

	/// Completes the file: closes the last cluster and, when the stream is
	/// seekable, adds the index.
	pub fn finish(&mut self) -> io::Result<()> {
		if self.finished {
			return Ok(());
		}
		self.flush_cluster()?;
		self.finished = true;

		let cues_position = self.stream.stream_position()?;
		if !self.cues.is_empty() {
			write_master(&mut self.stream, CUES, |body| {
				for (time_ms, track, cluster_position) in &self.cues {
					write_master(body, CUE_POINT, |point| {
						write_unsigned(point, CUE_TIME, *time_ms as u64)?;
						write_master(point, CUE_TRACK_POSITIONS, |positions| {
							write_unsigned(positions, CUE_TRACK, *track as u64)?;
							write_unsigned(positions, CUE_CLUSTER_POSITION, *cluster_position)
						})
					})?;
				}
				Ok(())
			})?;
		}

		let end = self.stream.stream_position()?;

		let mut seek_head = Vec::new();
		write_master(&mut seek_head, SEEK_HEAD, |body| {
			write_seek(body, INFO, self.info_position, self.segment_data_start)?;
			write_seek(body, TRACKS, self.tracks_position, self.segment_data_start)?;
			if !self.cues.is_empty() {
				write_seek(body, CUES, cues_position, self.segment_data_start)?;
			}
			Ok(())
		})?;
		if seek_head.len() + 9 <= SEEK_HEAD_RESERVED_BYTES {
			self.stream.seek(SeekFrom::Start(self.seek_head_position))?;
			self.stream.write_all(&seek_head)?;
			write_void(&mut self.stream, SEEK_HEAD_RESERVED_BYTES - seek_head.len())?;
		}

		self.stream.seek(SeekFrom::Start(self.duration_value_position))?;
		self.stream.write_all(&(self.last_timestamp_ms as f64).to_be_bytes())?;

		self.stream.seek(SeekFrom::Start(self.segment_size_position))?;
		write_size_exact(&mut self.stream, (end - self.segment_data_start) as i64, 8)?;

		self.stream.seek(SeekFrom::Start(end))?;
		self.stream.flush()
	}
}

fn nanoseconds_since_2001(created_utc: DateTime<Utc>) -> i64 {
	use chrono::NaiveDate;
	let epoch_2001 = NaiveDate::from_ymd_opt(2001, 1, 1)
		.and_then(|d| d.and_hms_opt(0, 0, 0))
		.map(|n| n.and_utc())
		.unwrap_or(DateTime::UNIX_EPOCH);
	(created_utc - epoch_2001).num_nanoseconds().unwrap_or(0)
}

fn write_seek<W: Write>(w: &mut W, id: u32, position: u64, segment_data_start: u64) -> io::Result<()> {
	write_master(w, SEEK, |seek| {
		write_binary(seek, SEEK_ID, &id_bytes(id))?;
		write_unsigned(seek, SEEK_POSITION, position - segment_data_start)
	})
}

fn write_track<W: Write>(entry: &mut W, track: &MatroskaTrack) -> io::Result<()> {
	write_unsigned(entry, TRACK_NUMBER, track.number as u64)?;
	write_unsigned(entry, TRACK_UID, track.number as u64)?;
	write_unsigned(entry, TRACK_TYPE, track.track_type as u64)?;
	write_unsigned(entry, FLAG_LACING, 0)?;
	write_string(entry, CODEC_ID, track.codec_id)?;
	if let Some(codec_private) = &track.codec_private
		&& !codec_private.is_empty()
	{
		write_binary(entry, CODEC_PRIVATE, codec_private)?;
	}
	if track.codec_delay_ns > 0 {
		write_unsigned(entry, CODEC_DELAY, track.codec_delay_ns)?;
	}
	if track.seek_pre_roll_ns > 0 {
		write_unsigned(entry, SEEK_PRE_ROLL, track.seek_pre_roll_ns)?;
	}
	if track.track_type == MatroskaTrackType::Video {
		write_master(entry, VIDEO, |video| {
			write_unsigned(video, PIXEL_WIDTH, track.pixel_width.max(1) as u64)?;
			write_unsigned(video, PIXEL_HEIGHT, track.pixel_height.max(1) as u64)
		})
	} else {
		write_master(entry, AUDIO, |audio| {
			write_float(audio, SAMPLING_FREQUENCY, track.sampling_frequency)?;
			write_unsigned(audio, CHANNELS, track.channels.max(1) as u64)
		})
	}
}

/// A finished file of a recording; `number` counts from 1 in the order files were written.
#[derive(Debug, Clone)]
pub struct LiveRecordingPart {
	pub number: u32,
	pub extension: String,
	pub duration_ms: i64,
	pub bytes: u64,
	pub has_video: bool,
}

struct PendingFrame {
	time_ms: i64,
	is_key_frame: bool,
	is_video: bool,
	data: Vec<u8>,
}

/// Turns live encoded video and Opus audio into Matroska files written as
/// they are received. Nothing is written until the first video key frame (or,
/// for audio-only broadcasts, a short wait), so every file starts decodable.
/// VP8 is stored as WebM and H.264 as Matroska; neither is re-encoded. When
/// the media can no longer be described by the open file's tracks (video
/// arriving after an audio-only start, or a different video codec after
/// reconnecting), that file is closed and a new part begins at the next key
/// frame, so no media is discarded.
/// Creates one output file for a part: `(part number, extension) -> file`.
pub type CreateOutput = Box<dyn FnMut(u32, &str) -> io::Result<std::fs::File> + Send>;
/// Called when a part's file is complete and closed.
pub type PartCompleted = Box<dyn FnMut(&LiveRecordingPart) + Send>;

pub struct LiveMuxer {
	create_output: CreateOutput,
	audio_channels: u32,
	video_timeline: RtpTimeline,
	audio_timeline: RtpTimeline,
	pending: Vec<PendingFrame>,
	parts: Vec<LiveRecordingPart>,
	writer: Option<MatroskaLiveWriter<std::fs::File>>,
	output: Option<std::fs::File>,
	extension: String,
	video_codec: Option<LiveVideoCodec>,
	audio_track: u32,
	origin_ms: i64,
	newest_ms: i64,
	first_audio_arrival_ms: i64,
	last_flush_ms: i64,
	finished: bool,
	/// Called when a part's file is complete and closed.
	pub part_completed: Option<PartCompleted>,
}

const VIDEO_TRACK: u32 = 1;
const INTERLEAVE_WINDOW_MS: i64 = 500;
const AUDIO_ONLY_AFTER_MS: i64 = 10_000;
const CLUSTER_FLUSH_INTERVAL_MS: i64 = 2000;

impl LiveMuxer {
	pub fn new(create_output: impl FnMut(u32, &str) -> io::Result<std::fs::File> + Send + 'static, audio_channels: u32) -> Self {
		Self {
			create_output: Box::new(create_output),
			audio_channels,
			video_timeline: RtpTimeline::new(90_000),
			audio_timeline: RtpTimeline::new(48_000),
			pending: Vec::new(),
			parts: Vec::new(),
			writer: None,
			output: None,
			extension: String::new(),
			video_codec: None,
			audio_track: 0,
			origin_ms: 0,
			newest_ms: i64::MIN,
			first_audio_arrival_ms: -1,
			last_flush_ms: 0,
			finished: false,
			part_completed: None,
		}
	}

	/// Whether any media has been written, in the open part or an earlier one.
	pub fn is_started(&self) -> bool {
		self.writer.is_some() || !self.parts.is_empty()
	}

	/// Whether a part is open now.
	pub fn is_writing(&self) -> bool {
		self.writer.is_some()
	}

	pub fn completed_parts(&self) -> &[LiveRecordingPart] {
		&self.parts
	}

	pub fn bytes_written(&mut self) -> io::Result<u64> {
		let mut total: u64 = self.parts.iter().map(|p| p.bytes).sum();
		if let Some(writer) = &mut self.writer {
			total += writer.bytes_written()?;
		}
		Ok(total)
	}

	pub fn duration_ms(&mut self) -> io::Result<i64> {
		let mut total: i64 = self.parts.iter().map(|p| p.duration_ms).sum();
		if let Some(writer) = &self.writer {
			total += writer.last_timestamp_ms();
		}
		Ok(total)
	}

	/// Adds a complete video frame. H.264 needs its decoder configuration
	/// (`codec_private`), so frames arriving before the parameter sets are
	/// ignored rather than starting an undecodable track.
	pub fn add_video_frame(&mut self, frame: EncodedVideoFrame, codec: LiveVideoCodec, codec_private: Option<&[u8]>, ssrc: u32, arrival_ms: i64) -> io::Result<()> {
		if self.finished || (codec == LiveVideoCodec::H264 && codec_private.is_none()) {
			return Ok(());
		}
		let ms = self.video_timeline.map(frame.rtp_timestamp, ssrc, arrival_ms);
		if self.writer.is_some() && self.video_codec != Some(codec) {
			// The open part cannot hold this video; switch files at a point the new part can start from.
			if !frame.is_key_frame {
				return Ok(());
			}
			self.close_part()?;
		}
		if self.writer.is_none() && !frame.is_key_frame {
			return Ok(());
		}
		if self.writer.is_none() {
			self.start(ms, Some(codec), codec_private.map(|c| c.to_vec()), frame.width, frame.height)?;
		}
		self.enqueue(PendingFrame { time_ms: ms, is_key_frame: frame.is_key_frame, is_video: true, data: frame.data })?;
		Ok(())
	}

	pub fn add_audio_frame(&mut self, opus_packet: Vec<u8>, rtp_timestamp: u32, ssrc: u32, arrival_ms: i64) -> io::Result<()> {
		if self.finished || opus_packet.is_empty() {
			return Ok(());
		}
		let ms = self.audio_timeline.map(rtp_timestamp, ssrc, arrival_ms);
		if self.first_audio_arrival_ms < 0 {
			self.first_audio_arrival_ms = arrival_ms;
		}
		if self.writer.is_none() && arrival_ms - self.first_audio_arrival_ms >= AUDIO_ONLY_AFTER_MS {
			let start_ms = self.pending.first().map(|f| f.time_ms).unwrap_or(ms);
			self.start(start_ms, None, None, 0, 0)?;
		}
		self.enqueue(PendingFrame { time_ms: ms, is_key_frame: true, is_video: false, data: opus_packet })?;
		Ok(())
	}

	/// Writes frames that are old enough to be in order and keeps the file
	/// current during quiet spells.
	pub fn pump(&mut self, now_ms: i64) -> io::Result<()> {
		if self.writer.is_none() {
			return Ok(());
		}
		let up_to = self.newest_ms - INTERLEAVE_WINDOW_MS;
		self.release(up_to)?;
		if now_ms - self.last_flush_ms >= CLUSTER_FLUSH_INTERVAL_MS {
			if let Some(writer) = &mut self.writer {
				writer.flush_cluster()?;
			}
			self.last_flush_ms = now_ms;
		}
		Ok(())
	}

	/// Closes the open part now; later media starts the next part (for example, a new stream).
	pub fn begin_new_part(&mut self) -> io::Result<()> {
		self.close_part()?;
		self.pending.clear();
		Ok(())
	}

	pub fn finish(&mut self) -> io::Result<()> {
		if self.finished {
			return Ok(());
		}
		self.close_part()?;
		self.pending.clear();
		self.finished = true;
		Ok(())
	}

	fn start(&mut self, origin_ms: i64, codec: Option<LiveVideoCodec>, codec_private: Option<Vec<u8>>, width: u32, height: u32) -> io::Result<()> {
		self.origin_ms = origin_ms;
		self.video_codec = codec;
		let mut tracks = Vec::new();
		self.extension = ".webm".to_string();
		if let Some(codec) = codec {
			let (codec_id, codec_private, extension) = match codec {
				LiveVideoCodec::H264 => ("V_MPEG4/ISO/AVC", codec_private, ".mkv"),
				LiveVideoCodec::Vp8 => ("V_VP8", None, ".webm"),
			};
			self.extension = extension.to_string();
			tracks.push(MatroskaTrack {
				number: VIDEO_TRACK,
				track_type: MatroskaTrackType::Video,
				codec_id,
				codec_private,
				pixel_width: width,
				pixel_height: height,
				sampling_frequency: 0.0,
				channels: 0,
				codec_delay_ns: 0,
				seek_pre_roll_ns: 0,
			});
		}
		self.audio_track = tracks.len() as u32 + 1;
		tracks.push(MatroskaTrack {
			number: self.audio_track,
			track_type: MatroskaTrackType::Audio,
			codec_id: "A_OPUS",
			codec_private: Some(build_opus_head(self.audio_channels)),
			pixel_width: 0,
			pixel_height: 0,
			sampling_frequency: 48_000.0,
			channels: self.audio_channels,
			codec_delay_ns: 6_500_000,
			seek_pre_roll_ns: 80_000_000,
		});

		let doc_type = if codec == Some(LiveVideoCodec::H264) { "matroska" } else { "webm" };
		let output = (self.create_output)(self.parts.len() as u32 + 1, &self.extension)?;
		self.writer = Some(MatroskaLiveWriter::new(output.try_clone()?, doc_type, &tracks, Utc::now())?);
		self.output = Some(output);

		// Audio received before the first key frame cannot be shown with video, so the part starts at the key frame.
		self.pending.retain(|frame| frame.time_ms >= origin_ms);
		Ok(())
	}

	fn close_part(&mut self) -> io::Result<()> {
		if self.writer.is_none() {
			return Ok(());
		}
		self.release(i64::MAX)?;
		let Some(mut writer) = self.writer.take() else {
			return Ok(());
		};
		writer.finish()?;
		let bytes = self.output.as_mut().map(|f| f.metadata().map(|m| m.len()).unwrap_or(0)).unwrap_or(0);
		if let Some(output) = &mut self.output {
			output.flush()?;
		}
		let part = LiveRecordingPart {
			number: self.parts.len() as u32 + 1,
			extension: std::mem::take(&mut self.extension),
			duration_ms: writer.last_timestamp_ms(),
			bytes,
			has_video: self.video_codec.is_some(),
		};
		self.output = None;
		self.video_codec = None;
		self.first_audio_arrival_ms = -1;
		if let Some(callback) = &mut self.part_completed {
			callback(&part);
		}
		self.parts.push(part);
		Ok(())
	}

	fn enqueue(&mut self, frame: PendingFrame) -> io::Result<()> {
		let mut index = self.pending.len();
		while index > 0 && self.pending[index - 1].time_ms > frame.time_ms {
			index -= 1;
		}
		self.pending.insert(index, frame);
		self.newest_ms = self.newest_ms.max(self.pending.last().map(|f| f.time_ms).unwrap_or(i64::MIN));
		if self.writer.is_some() {
			let up_to = self.newest_ms - INTERLEAVE_WINDOW_MS;
			self.release(up_to)?;
		}
		Ok(())
	}

	fn release(&mut self, up_to_ms: i64) -> io::Result<()> {
		let Some(writer) = &mut self.writer else {
			return Ok(());
		};
		let mut count = 0;
		while count < self.pending.len() && self.pending[count].time_ms <= up_to_ms {
			let frame = &self.pending[count];
			if frame.time_ms < self.origin_ms || (frame.is_video && self.video_codec.is_none()) {
				count += 1;
				continue;
			}
			let track = if frame.is_video { VIDEO_TRACK } else { self.audio_track };
			writer.write_frame(track, frame.time_ms - self.origin_ms, frame.is_key_frame, frame.is_video, &frame.data)?;
			count += 1;
		}
		self.pending.drain(0..count);
		Ok(())
	}
}

fn build_opus_head(channels: u32) -> Vec<u8> {
	let mut head = vec![0u8; 19];
	head[..8].copy_from_slice(b"OpusHead");
	head[8] = 1; // version
	head[9] = channels as u8;
	// Pre-skip 0: the stream joins mid-flow, so there is no encoder start-up to hide.
	head[12..16].copy_from_slice(&48_000u32.to_le_bytes());
	head
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::livestream::rtp::{EncodedVideoFrame, H264Depacketizer, LiveVideoCodec};

	fn vp8_key_frame() -> EncodedVideoFrame {
		let mut data = vec![0x00, 0x00, 0x00]; // frame tag: key frame
		data.extend_from_slice(&[0x9D, 0x01, 0x2A, 0x80, 0x02, 0x38, 0x01]); // start code + 640x312
		data.extend_from_slice(&[0xAA; 32]);
		EncodedVideoFrame { data, rtp_timestamp: 1000, is_key_frame: true, width: 640, height: 312 }
	}

	#[test]
	fn muxer_writes_a_playable_webm_part() {
		let dir = tempfile::tempdir().unwrap();
		let dir_path = dir.path().to_path_buf();
		let parts = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
		let parts_for_cb = parts.clone();
		let mut muxer = LiveMuxer::new(
			move |part, extension| {
				let path = dir_path.join(format!("part{part}{extension}"));
				std::fs::File::create(&path)
			},
			2,
		);
		muxer.part_completed = Some(Box::new(move |part| parts_for_cb.lock().unwrap().push(part.clone())));

		// Non-key video is dropped until a key frame arrives.
		let mut non_key = vp8_key_frame();
		non_key.is_key_frame = false;
		muxer.add_video_frame(non_key, LiveVideoCodec::Vp8, None, 1, 1000).unwrap();
		assert!(!muxer.is_started());

		muxer.add_video_frame(vp8_key_frame(), LiveVideoCodec::Vp8, None, 1, 1000).unwrap();
		assert!(muxer.is_started());
		muxer.add_audio_frame(vec![0x11; 20], 48_000, 2, 1010).unwrap();
		muxer.pump(2000).unwrap();
		muxer.finish().unwrap();

		let parts = parts.lock().unwrap();
		assert_eq!(parts.len(), 1);
		assert_eq!(parts[0].extension, ".webm");
		assert!(parts[0].has_video);
		let path = dir.path().join("part1.webm");
		let bytes = std::fs::read(&path).unwrap();
		// EBML header id + "webm" doctype + a Cluster + SimpleBlock must be present.
		assert_eq!(&bytes[0..4], &0x1A45DFA3u32.to_be_bytes());
		let text = String::from_utf8_lossy(&bytes);
		assert!(text.contains("webm"));
		assert!(bytes.windows(4).any(|w| w == 0x1F43B675u32.to_be_bytes())); // Cluster
		assert!(bytes.windows(2).any(|w| w == 0xA3u16.to_be_bytes())); // SimpleBlock
		assert!(bytes.windows(4).any(|w| w == 0x1C53BB6Bu32.to_be_bytes())); // Cues written by finish
	}

	#[test]
	fn muxer_switches_to_mkv_for_h264() {
		let dir = tempfile::tempdir().unwrap();
		let dir_path = dir.path().to_path_buf();
		let mut muxer = LiveMuxer::new(move |part, extension| std::fs::File::create(dir_path.join(format!("part{part}{extension}"))), 2);
		let sps = vec![0x67, 0x64, 0x00, 0x1F, 0xAC];
		let pps = vec![0x68, 0xEB];
		let avcc = H264Depacketizer::build_decoder_configuration(&sps, &pps);
		let mut frame = vp8_key_frame();
		frame.data = {
			let mut d = Vec::new();
			d.extend_from_slice(&(sps.len() as u32).to_be_bytes());
			d.extend_from_slice(&sps);
			d.extend_from_slice(&(pps.len() as u32).to_be_bytes());
			d.extend_from_slice(&pps);
			d.extend_from_slice(&4u32.to_be_bytes());
			d.extend_from_slice(&[0x65, 0x88, 0x84, 0x00]);
			d
		};
		muxer.add_video_frame(frame, LiveVideoCodec::H264, Some(&avcc), 1, 1000).unwrap();
		muxer.finish().unwrap();
		assert!(dir.path().join("part1.mkv").exists());
		let bytes = std::fs::read(dir.path().join("part1.mkv")).unwrap();
		let text = String::from_utf8_lossy(&bytes);
		assert!(text.contains("matroska"));
		assert!(text.contains("V_MPEG4/ISO/AVC"));
	}
}
