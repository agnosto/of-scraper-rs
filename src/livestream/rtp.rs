//! RTP stream handling for live recordings: reordering, retransmission-gap
//! detection, VP8/H.264 depacketizing and RTP-timestamp mapping. Ported from
//! the protocol-neutral media layer of SubScraper (`SubScraper.Core/Media`).

/// Video codec of a broadcast, chosen by the payload type of the offer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LiveVideoCodec {
	Vp8,
	H264,
}

/// The parts of a received RTP packet that reordering and depacketizing need.
#[derive(Debug, Clone)]
pub struct RtpPacketInfo {
	pub sequence_number: u16,
	pub timestamp: u32,
	pub marker: bool,
	pub ssrc: u32,
	pub payload: Vec<u8>,
}

/// One complete encoded picture. H.264 data is in length-prefixed (AVCC)
/// form; dimensions are zero when the frame does not carry them.
#[derive(Debug, Clone)]
pub struct EncodedVideoFrame {
	pub data: Vec<u8>,
	pub rtp_timestamp: u32,
	pub is_key_frame: bool,
	pub width: u32,
	pub height: u32,
}

/// Rebuilds encoded frames from in-order RTP payloads of one video stream.
pub trait VideoDepacketizer {
	fn codec(&self) -> LiveVideoCodec;
	/// Adds the next packet and returns a frame when one completes.
	fn push(&mut self, packet: &RtpPacketInfo) -> Option<EncodedVideoFrame>;
	/// Discards any partly assembled frame after packets were lost.
	fn reset(&mut self);
	/// The container's decoder configuration when the parameter sets are known (H.264 only).
	fn decoder_configuration(&self) -> Option<Vec<u8>> {
		None
	}
}

/// Releases the packets of one RTP stream in sequence order. A missing packet
/// is waited for briefly, giving a retransmission time to arrive, before the
/// gap is reported as lost and later packets are released.
pub struct RtpReorderBuffer {
	pending: std::collections::BTreeMap<i64, (RtpPacketInfo, i64)>,
	next: i64,
	highest: i64,
	max_buffered_packets: usize,
	max_wait_ms: i64,
}

impl Default for RtpReorderBuffer {
	fn default() -> Self {
		Self::new()
	}
}

impl RtpReorderBuffer {
	// Offset keeps extended sequence numbers positive when packets older than
	// the first one arrive.
	const INITIAL_EXTENSION: i64 = 1 << 20;
	const MAX_REPORTED_MISSING: usize = 64;

	pub fn new() -> Self {
		Self {
			pending: Default::default(),
			next: -1,
			highest: -1,
			max_buffered_packets: 512,
			max_wait_ms: 200,
		}
	}

	/// Adds a packet, returning the packets released in order and the sequence
	/// numbers newly found missing (so they can be requested again).
	pub fn push(&mut self, packet: RtpPacketInfo, now_ms: i64) -> (Vec<RtpPacketInfo>, Vec<u16>) {
		let extended = self.extend(packet.sequence_number);
		if self.next < 0 {
			self.next = extended;
		}
		if extended < self.next || self.pending.contains_key(&extended) {
			return (Vec::new(), Vec::new());
		}

		let mut missing = Vec::new();
		if extended > self.highest {
			let from = (self.highest + 1).max(self.next);
			let mut sequence = from;
			while sequence < extended && missing.len() < Self::MAX_REPORTED_MISSING {
				missing.push(sequence as u16);
				sequence += 1;
			}
			self.highest = extended;
		}

		self.pending.insert(extended, (packet, now_ms));
		let (released, _) = self.release(now_ms);
		(released, missing)
	}

	/// Gives up on gaps that have waited too long. Call periodically so a
	/// stalled stream still drains.
	pub fn flush(&mut self, now_ms: i64) -> (Vec<RtpPacketInfo>, Vec<i32>) {
		self.release(now_ms)
	}

	/// Releases everything still buffered, reporting the remaining gaps as lost.
	pub fn drain(&mut self) -> (Vec<RtpPacketInfo>, Vec<i32>) {
		self.release(i64::MAX)
	}

	fn release(&mut self, now_ms: i64) -> (Vec<RtpPacketInfo>, Vec<i32>) {
		let mut released = Vec::new();
		let mut losses = Vec::new();
		loop {
			while let Some((packet, _)) = self.pending.remove(&self.next) {
				released.push(packet);
				self.next += 1;
			}
			if self.pending.is_empty() {
				return (released, losses);
			}
			let (oldest_key, arrival_ms) = self.pending.first_key_value().map(|(k, v)| (*k, v.1)).unwrap();
			if self.pending.len() <= self.max_buffered_packets && now_ms - arrival_ms < self.max_wait_ms {
				return (released, losses);
			}
			losses.push((oldest_key - self.next) as i32);
			self.next = oldest_key;
		}
	}

	fn extend(&self, sequence_number: u16) -> i64 {
		if self.highest < 0 {
			return Self::INITIAL_EXTENSION + sequence_number as i64;
		}
		let delta = sequence_number.wrapping_sub(self.highest as u16) as i16;
		self.highest + delta as i64
	}
}

/// Reassembles VP8 frames from RTP payloads (RFC 7741).
pub struct Vp8Depacketizer {
	frame: Vec<u8>,
	timestamp: u32,
	in_frame: bool,
}

impl Default for Vp8Depacketizer {
	fn default() -> Self {
		Self::new()
	}
}

impl Vp8Depacketizer {
	pub fn new() -> Self {
		Self { frame: Vec::new(), timestamp: 0, in_frame: false }
	}

	/// Returns the payload offset past the VP8 payload descriptor and whether
	/// the packet starts a frame.
	fn try_read_descriptor(payload: &[u8]) -> Option<(usize, bool)> {
		if payload.is_empty() {
			return None;
		}
		let first = payload[0];
		let mut index = 1usize;
		if first & 0x80 != 0 {
			if payload.len() <= index {
				return None;
			}
			let extension = payload[index];
			index += 1;
			if extension & 0x80 != 0 {
				if payload.len() <= index {
					return None;
				}
				// A set M bit means a 15-bit picture ID.
				index += if payload[index] & 0x80 != 0 { 2 } else { 1 };
			}
			if extension & 0x40 != 0 {
				index += 1;
			}
			if extension & 0x30 != 0 {
				index += 1;
			}
		}
		if index > payload.len() {
			return None;
		}
		let start_of_frame = first & 0x10 != 0 && first & 0x07 == 0;
		Some((index, start_of_frame))
	}
}

impl VideoDepacketizer for Vp8Depacketizer {
	fn codec(&self) -> LiveVideoCodec {
		LiveVideoCodec::Vp8
	}

	fn push(&mut self, packet: &RtpPacketInfo) -> Option<EncodedVideoFrame> {
		let (offset, start_of_frame) = Self::try_read_descriptor(&packet.payload)?;
		if start_of_frame {
			self.frame.clear();
			self.timestamp = packet.timestamp;
			self.in_frame = true;
		} else if !self.in_frame || packet.timestamp != self.timestamp {
			self.reset();
			return None;
		}

		self.frame.extend_from_slice(&packet.payload[offset..]);
		if !packet.marker {
			return None;
		}
		self.in_frame = false;
		if self.frame.is_empty() {
			return None;
		}
		let data = std::mem::take(&mut self.frame);
		// The inverse key frame flag is the lowest bit of the frame tag.
		let is_key_frame = data[0] & 0x01 == 0;
		let (mut width, mut height) = (0, 0);
		if is_key_frame && data.len() >= 10 && data[3] == 0x9D && data[4] == 0x01 && data[5] == 0x2A {
			width = (data[6] as u32 | (data[7] as u32) << 8) & 0x3FFF;
			height = (data[8] as u32 | (data[9] as u32) << 8) & 0x3FFF;
		}
		Some(EncodedVideoFrame { data, rtp_timestamp: self.timestamp, is_key_frame, width, height })
	}

	fn reset(&mut self) {
		self.in_frame = false;
		self.frame.clear();
	}
}

/// Reassembles H.264 access units from RTP payloads (RFC 6184 single NAL
/// units, STAP-A and FU-A) into length-prefixed frames, keeping the latest
/// parameter sets for the container's decoder configuration.
pub struct H264Depacketizer {
	nal_units: Vec<Vec<u8>>,
	fragment: Option<Vec<u8>>,
	timestamp: u32,
	in_frame: bool,
	corrupt: bool,
	sps: Option<Vec<u8>>,
	pps: Option<Vec<u8>>,
}

const NAL_TYPE_IDR: u8 = 5;
const NAL_TYPE_SPS: u8 = 7;
const NAL_TYPE_PPS: u8 = 8;
const NAL_TYPE_STAP_A: u8 = 24;
const NAL_TYPE_FU_A: u8 = 28;

impl Default for H264Depacketizer {
	fn default() -> Self {
		Self::new()
	}
}

impl H264Depacketizer {
	pub fn new() -> Self {
		Self {
			nal_units: Vec::new(),
			fragment: None,
			timestamp: 0,
			in_frame: false,
			corrupt: false,
			sps: None,
			pps: None,
		}
	}

	pub fn sps(&self) -> Option<&[u8]> {
		self.sps.as_deref()
	}

	pub fn pps(&self) -> Option<&[u8]> {
		self.pps.as_deref()
	}

	/// Builds the AVCDecoderConfigurationRecord used as Matroska codec private data.
	pub fn build_decoder_configuration(sps: &[u8], pps: &[u8]) -> Vec<u8> {
		let mut out = Vec::with_capacity(sps.len() + pps.len() + 8);
		out.push(1);
		out.push(if sps.len() > 1 { sps[1] } else { 0x42 });
		out.push(if sps.len() > 2 { sps[2] } else { 0 });
		out.push(if sps.len() > 3 { sps[3] } else { 0x1F });
		out.push(0xFF); // four-byte NAL lengths
		out.push(0xE1); // one SPS
		out.extend_from_slice(&(sps.len() as u16).to_be_bytes());
		out.extend_from_slice(sps);
		out.push(1);
		out.extend_from_slice(&(pps.len() as u16).to_be_bytes());
		out.extend_from_slice(pps);
		out
	}

	fn add_nal_unit(&mut self, nal_unit: Vec<u8>) {
		if nal_unit.is_empty() {
			return;
		}
		let nal_type = nal_unit[0] & 0x1F;
		if nal_type == NAL_TYPE_SPS {
			self.sps = Some(nal_unit.clone());
		} else if nal_type == NAL_TYPE_PPS {
			self.pps = Some(nal_unit.clone());
		}
		self.nal_units.push(nal_unit);
	}

	fn build_frame(&self) -> EncodedVideoFrame {
		let mut is_key_frame = false;
		let mut stream = Vec::new();
		for nal_unit in &self.nal_units {
			is_key_frame |= (nal_unit[0] & 0x1F) == NAL_TYPE_IDR;
			stream.extend_from_slice(&(nal_unit.len() as u32).to_be_bytes());
			stream.extend_from_slice(nal_unit);
		}
		let (width, height) = if is_key_frame
			&& let Some(sps) = &self.sps
		{
			H264SpsParser::try_get_dimensions(sps)
		} else {
			(0, 0)
		};
		EncodedVideoFrame { data: stream, rtp_timestamp: self.timestamp, is_key_frame, width, height }
	}
}

impl VideoDepacketizer for H264Depacketizer {
	fn codec(&self) -> LiveVideoCodec {
		LiveVideoCodec::H264
	}

	fn push(&mut self, packet: &RtpPacketInfo) -> Option<EncodedVideoFrame> {
		let payload = &packet.payload;
		if payload.is_empty() {
			return None;
		}
		if self.in_frame && packet.timestamp != self.timestamp {
			// The previous access unit never saw its marker packet.
			self.reset();
		}
		if !self.in_frame {
			self.in_frame = true;
			self.corrupt = false;
			self.timestamp = packet.timestamp;
		}

		let nal_type = payload[0] & 0x1F;
		if (1..=23).contains(&nal_type) {
			self.add_nal_unit(payload.to_vec());
		} else if nal_type == NAL_TYPE_STAP_A {
			let mut index = 1usize;
			while index + 2 <= payload.len() {
				let size = u16::from_be_bytes([payload[index], payload[index + 1]]) as usize;
				index += 2;
				if size == 0 || index + size > payload.len() {
					self.corrupt = true;
					break;
				}
				self.add_nal_unit(payload[index..index + size].to_vec());
				index += size;
			}
		} else if nal_type == NAL_TYPE_FU_A && payload.len() >= 2 {
			let header = payload[1];
			if header & 0x80 != 0 {
				let mut fragment = Vec::with_capacity(payload.len());
				fragment.push((payload[0] & 0xE0) | (header & 0x1F));
				self.fragment = Some(fragment);
			}
			if let Some(fragment) = &mut self.fragment {
				fragment.extend_from_slice(&payload[2..]);
				if header & 0x40 != 0 {
					let done = std::mem::take(fragment);
					self.fragment = None;
					self.add_nal_unit(done);
				}
			} else {
				self.corrupt = true;
			}
		}

		if !packet.marker {
			return None;
		}
		let frame = if self.corrupt || self.fragment.is_some() || self.nal_units.is_empty() {
			None
		} else {
			Some(self.build_frame())
		};
		self.reset();
		frame
	}

	fn reset(&mut self) {
		self.in_frame = false;
		self.corrupt = false;
		self.fragment = None;
		self.nal_units.clear();
	}

	fn decoder_configuration(&self) -> Option<Vec<u8>> {
		match (self.sps(), self.pps()) {
			(Some(sps), Some(pps)) => Some(Self::build_decoder_configuration(sps, pps)),
			_ => None,
		}
	}
}

/// Reads the coded picture size from an H.264 sequence parameter set.
pub struct H264SpsParser;

impl H264SpsParser {
	const HIGH_PROFILES: [u8; 13] = [100, 110, 122, 244, 44, 83, 86, 118, 128, 138, 139, 134, 135];

	/// Returns the cropped width and height, or (0, 0) when the SPS cannot be read.
	pub fn try_get_dimensions(sps: &[u8]) -> (u32, u32) {
		let data = remove_emulation_prevention(sps);
		let mut reader = BitReader { data: &data, position: 0 };
		reader.try_get_dimensions().unwrap_or((0, 0))
	}
}

fn remove_emulation_prevention(data: &[u8]) -> Vec<u8> {
	let mut output = Vec::with_capacity(data.len());
	let mut zeros = 0;
	for &value in data {
		if zeros >= 2 && value == 0x03 {
			zeros = 0;
			continue;
		}
		output.push(value);
		zeros = if value == 0 { zeros + 1 } else { 0 };
	}
	output
}

struct BitReader<'a> {
	data: &'a [u8],
	position: usize,
}

impl<'a> BitReader<'a> {
	fn read(&mut self, bits: usize) -> Option<u32> {
		let mut value = 0u32;
		for _ in 0..bits {
			let byte = *self.data.get(self.position / 8)?;
			let bit = (byte >> (7 - (self.position % 8))) & 1;
			value = (value << 1) | bit as u32;
			self.position += 1;
		}
		Some(value)
	}

	fn skip(&mut self, bits: usize) -> Option<()> {
		self.position += bits;
		if self.position > self.data.len() * 8 { None } else { Some(()) }
	}

	fn read_unsigned_exp_golomb(&mut self) -> Option<u32> {
		let mut leading_zeros = 0;
		while self.read(1)? == 0 {
			leading_zeros += 1;
			if leading_zeros > 31 {
				return None;
			}
		}
		if leading_zeros == 0 {
			return Some(0);
		}
		Some((1u32 << leading_zeros) - 1 + self.read(leading_zeros)?)
	}

	fn read_signed_exp_golomb(&mut self) -> Option<i32> {
		let value = self.read_unsigned_exp_golomb()? as i32;
		Some(if value & 1 == 1 { (value + 1) / 2 } else { -(value / 2) })
	}

	fn skip_scaling_lists(&mut self, count: usize) -> Option<()> {
		for list in 0..count {
			if self.read(1)? == 0 {
				continue;
			}
			let size = if list < 6 { 16 } else { 64 };
			let mut last = 8i32;
			let mut next = 8i32;
			for _ in 0..size {
				if next != 0 {
					next = (last + self.read_signed_exp_golomb()? + 256) % 256;
				}
				last = if next == 0 { last } else { next };
			}
		}
		Some(())
	}

	fn try_get_dimensions(&mut self) -> Option<(u32, u32)> {
		self.skip(8)?; // NAL header
		let profile = self.read(8)? as u8;
		self.skip(16)?; // constraint flags and level
		self.read_unsigned_exp_golomb()?; // seq_parameter_set_id

		let mut chroma_format = 1i32;
		let mut separate_colour_planes = false;
		if H264SpsParser::HIGH_PROFILES.contains(&profile) {
			chroma_format = self.read_unsigned_exp_golomb()? as i32;
			if chroma_format == 3 {
				separate_colour_planes = self.read(1)? == 1;
			}
			self.read_unsigned_exp_golomb()?; // bit_depth_luma_minus8
			self.read_unsigned_exp_golomb()?; // bit_depth_chroma_minus8
			self.skip(1)?; // qpprime_y_zero_transform_bypass_flag
			if self.read(1)? == 1 {
				self.skip_scaling_lists(if chroma_format == 3 { 12 } else { 8 })?;
			}
		}

		self.read_unsigned_exp_golomb()?; // log2_max_frame_num_minus4
		let picture_order_count_type = self.read_unsigned_exp_golomb()?;
		if picture_order_count_type == 0 {
			self.read_unsigned_exp_golomb()?;
		} else if picture_order_count_type == 1 {
			self.skip(1)?;
			self.read_signed_exp_golomb()?;
			self.read_signed_exp_golomb()?;
			let cycle = self.read_unsigned_exp_golomb()?;
			for _ in 0..cycle {
				self.read_signed_exp_golomb()?;
			}
		}

		self.read_unsigned_exp_golomb()?; // max_num_ref_frames
		self.skip(1)?; // gaps_in_frame_num_value_allowed_flag
		let width_in_macroblocks = self.read_unsigned_exp_golomb()? + 1;
		let height_in_map_units = self.read_unsigned_exp_golomb()? + 1;
		let frame_macroblocks_only = self.read(1)?;
		if frame_macroblocks_only == 0 {
			self.skip(1)?; // mb_adaptive_frame_field_flag
		}
		self.skip(1)?; // direct_8x8_inference_flag
		let (mut crop_left, mut crop_right, mut crop_top, mut crop_bottom) = (0u32, 0u32, 0u32, 0u32);
		if self.read(1)? == 1 {
			crop_left = self.read_unsigned_exp_golomb()?;
			crop_right = self.read_unsigned_exp_golomb()?;
			crop_top = self.read_unsigned_exp_golomb()?;
			crop_bottom = self.read_unsigned_exp_golomb()?;
		}

		let chroma_array_type = if separate_colour_planes { 0 } else { chroma_format };
		let crop_unit_x: i64 = if chroma_array_type == 0 || chroma_array_type == 3 { 1 } else { 2 };
		let frame_macroblocks_only = frame_macroblocks_only as i64;
		let crop_unit_y: i64 = (if chroma_array_type == 1 { 2 } else { 1 }) * (2 - frame_macroblocks_only);
		let width = width_in_macroblocks as i64 * 16 - crop_unit_x * (crop_left + crop_right) as i64;
		let height = (2 - frame_macroblocks_only) * height_in_map_units as i64 * 16
			- crop_unit_y * (crop_top + crop_bottom) as i64;
		if width > 0 && height > 0 {
			Some((width as u32, height as u32))
		} else {
			None
		}
	}
}

/// Maps one track's RTP timestamps onto the recording clock in milliseconds.
/// The first packet of each stream (or after a discontinuity) is anchored to
/// its arrival time, so separately clocked tracks share one timeline and a
/// reconnection gap shows as a gap rather than overlapping media.
pub struct RtpTimeline {
	clock_rate: u32,
	started: bool,
	ssrc: u32,
	anchor_extended: u32,
	anchor_ms: i64,
	last_extended: u32,
	last_ms: i64,
}

impl RtpTimeline {
	const MAX_DRIFT_MS: i64 = 3000;

	pub fn new(clock_rate: u32) -> Self {
		Self {
			clock_rate,
			started: false,
			ssrc: 0,
			anchor_extended: 0,
			anchor_ms: 0,
			last_extended: 0,
			last_ms: i64::MIN,
		}
	}

	pub fn map(&mut self, rtp_timestamp: u32, ssrc: u32, arrival_ms: i64) -> i64 {
		if !self.started || ssrc != self.ssrc {
			return self.anchor(rtp_timestamp, ssrc, arrival_ms);
		}
		let extended = self.last_extended.wrapping_add(rtp_timestamp.wrapping_sub(self.last_extended));
		let ms = self.anchor_ms + (extended.wrapping_sub(self.anchor_extended)) as i64 * 1000 / self.clock_rate as i64;
		if (ms - arrival_ms).abs() > Self::MAX_DRIFT_MS {
			return self.anchor(rtp_timestamp, ssrc, arrival_ms);
		}
		self.last_extended = extended;
		self.last_ms = ms.max(self.last_ms);
		self.last_ms
	}

	fn anchor(&mut self, rtp_timestamp: u32, ssrc: u32, arrival_ms: i64) -> i64 {
		self.started = true;
		self.ssrc = ssrc;
		self.anchor_extended = rtp_timestamp;
		self.last_extended = rtp_timestamp;
		self.anchor_ms = arrival_ms.max(if self.last_ms == i64::MIN { arrival_ms } else { self.last_ms + 1 });
		self.last_ms = self.anchor_ms;
		self.anchor_ms
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn packet(seq: u16, ts: u32, marker: bool, payload: &[u8]) -> RtpPacketInfo {
		RtpPacketInfo { sequence_number: seq, timestamp: ts, marker, ssrc: 1, payload: payload.to_vec() }
	}

	#[test]
	fn reorder_buffer_releases_in_order_and_reports_gaps() {
		let mut buffer = RtpReorderBuffer::new();
		// The first packet defines the start and is released immediately.
		let (released, missing) = buffer.push(packet(2, 200, false, &[2]), 1000);
		assert_eq!(released.iter().map(|p| p.sequence_number).collect::<Vec<_>>(), vec![2]);
		assert!(missing.is_empty());
		// seq 4 arrives before 3: 3 is reported missing, 4 waits for the gap.
		let (released, missing) = buffer.push(packet(4, 400, true, &[4]), 1001);
		assert_eq!(missing, vec![3]);
		assert!(released.is_empty());
		// Once the gap has waited out, 4 is released and the gap is reported lost.
		let (released, losses) = buffer.flush(1001 + 500);
		assert_eq!(released.iter().map(|p| p.sequence_number).collect::<Vec<_>>(), vec![4]);
		assert_eq!(losses, vec![1]);
	}

	#[test]
	fn vp8_keyframe_dimensions_are_read() {
		// Minimal VP8 key frame: descriptor byte 0x10 (S=1, PID=0), then the
		// frame tag with key-frame bit clear, start code 9d 01 2a, then w/h.
		let mut payload = vec![0x10];
		payload.extend_from_slice(&[0x00, 0x00, 0x00]); // frame tag (key frame)
		payload.extend_from_slice(&[0x9D, 0x01, 0x2A]);
		payload.extend_from_slice(&[0x80, 0x02, 0x38, 0x01]); // 640x312 & 0x3FFF
		let mut depacketizer = Vp8Depacketizer::new();
		let frame = depacketizer.push(&packet(1, 90, true, &payload)).unwrap();
		assert!(frame.is_key_frame);
		assert_eq!((frame.width, frame.height), (640, 312));
	}

	#[test]
	fn h264_depacketizer_handles_single_nal_and_builds_avcc() {
		let sps = vec![0x67, 0x42, 0x00, 0x1F];
		let pps = vec![0x68, 0xCE];
		let idr = vec![0x65, 0x88, 0x84];
		let mut depacketizer = H264Depacketizer::new();
		assert!(depacketizer.push(&packet(1, 100, false, &sps)).is_none());
		assert!(depacketizer.push(&packet(2, 100, false, &pps)).is_none());
		let frame = depacketizer.push(&packet(3, 100, true, &idr)).unwrap();
		assert!(frame.is_key_frame);
		// length-prefixed NAL units
		assert_eq!(&frame.data[0..4], &4u32.to_be_bytes());
		assert_eq!(depacketizer.sps(), Some(sps.as_slice()));
		let avcc = H264Depacketizer::build_decoder_configuration(&sps, &pps);
		assert_eq!(avcc[0], 1);
		assert_eq!(avcc[4], 0xFF);
		assert_eq!(avcc[5], 0xE1);
	}

	#[test]
	fn timeline_anchors_and_extends() {
		let mut timeline = RtpTimeline::new(90_000);
		let first = timeline.map(1000, 7, 5000);
		assert_eq!(first, 5000);
		// +900 ticks = +10 ms
		let second = timeline.map(1900, 7, 5010);
		assert_eq!(second, 5010);
		// a far-off arrival re-anchors instead of drifting
		let third = timeline.map(1900 + 90_000 * 10, 7, 999_999);
		assert_eq!(third, 999_999);
	}
}
