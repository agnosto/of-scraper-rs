//! One receive-only viewing session of a broadcast: edge discovery, gateway
//! join and subscription, and a WebRTC peer (webrtc-rs) that delivers the
//! publisher's encoded RTP media without decoding it. Ported from
//! SubScraper's `AgoraLiveSession`.

use std::collections::HashMap;
use std::sync::Arc;

use log::{debug, info, warn};
use of_client::livestream::AgoraCredentials;
use serde_json::Value;
use tokio::sync::{mpsc, watch};
use webrtc::api::media_engine::MediaEngine;
use webrtc::api::APIBuilder;
use webrtc::ice_transport::ice_connection_state::RTCIceConnectionState;
use webrtc::peer_connection::configuration::RTCConfiguration;
use webrtc::peer_connection::peer_connection_state::RTCPeerConnectionState;
use webrtc::peer_connection::sdp::session_description::RTCSessionDescription;
use webrtc::peer_connection::RTCPeerConnection;
use webrtc::rtcp::payload_feedbacks::picture_loss_indication::PictureLossIndication;
use webrtc::rtcp::transport_feedbacks::transport_layer_nack::{NackPair, TransportLayerNack};
use webrtc::rtp_transceiver::rtp_codec::{RTCRtpCodecCapability, RTCRtpCodecParameters, RTPCodecType};
use webrtc::rtp_transceiver::rtp_transceiver_direction::RTCRtpTransceiverDirection;
use webrtc::rtp_transceiver::{RTCRtpTransceiverInit, RTCPFeedback};

use super::protocol::{self, AgoraEdge, AgoraGatewayInfo};
use super::rtp::{LiveVideoCodec, RtpPacketInfo};
use super::socket::GatewaySocket;

const VP8_PAYLOAD_TYPE: u8 = 96;
const H264_BASELINE_PAYLOAD_TYPE: u8 = 102;
const H264_HIGH_PAYLOAD_TYPE: u8 = 108;
const OPUS_PAYLOAD_TYPE: u8 = 111;

const VIDEO_FEEDBACK: [&str; 4] = ["transport-cc", "nack", "nack pli", "ccm fir"];
const AUDIO_FEEDBACK: [&str; 2] = ["transport-cc", "nack"];

/// Which gateway stream kind a retransmission request is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaKind {
	Audio,
	Video,
}

/// One connected viewing session. Media and events arrive on the channels;
/// `disconnected` resolves once the media or gateway connection is lost.
pub struct LiveSession {
	pub peer: Arc<RTCPeerConnection>,
	pub socket: Arc<GatewaySocket>,
	pub video_rx: mpsc::UnboundedReceiver<(RtpPacketInfo, LiveVideoCodec)>,
	pub audio_rx: mpsc::UnboundedReceiver<RtpPacketInfo>,
	/// `true` when the broadcaster left the channel, `false` when they (re)appeared.
	pub presence_rx: mpsc::UnboundedReceiver<bool>,
	/// `None` while connected, `Some(reason)` once the media or gateway connection is lost.
	pub disconnected_rx: watch::Receiver<Option<String>>,
}

impl LiveSession {
	/// Joins the broadcast described by `credentials`, trying up to three gateway edges.
	pub async fn connect(
		credentials: &AgoraCredentials,
		user_agent: &str,
		http: &reqwest::Client,
	) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
		let session_id = format!("{:032x}", rand::random::<u128>());
		let gateway = choose_server(http, credentials, &session_id).await?;

		let peer = create_peer().await?;
		let offer = peer.create_offer(None).await?;
		peer.set_local_description(offer).await?;
		let local = peer.local_description().await.ok_or("no local description after set_local_description")?;
		let offer_sdp = protocol::parse_offer(&local.sdp);
		let ortc = protocol::build_local_ortc(&offer_sdp).map_err(|e| e.to_string())?;

		// Payload type -> codec, taken from the offer so the answer and the packet routing agree.
		let mut video_payload_types: HashMap<u8, LiveVideoCodec> = HashMap::new();
		if let Some(video) = offer_sdp.media.iter().find(|m| m.kind == "video") {
			for (pt, format) in &video.formats {
				if format.name.eq_ignore_ascii_case("vp8") {
					video_payload_types.insert(*pt, LiveVideoCodec::Vp8);
				} else if format.name.eq_ignore_ascii_case("h264") {
					video_payload_types.insert(*pt, LiveVideoCodec::H264);
				}
			}
		}
		let opus_payload_type = offer_sdp
			.media
			.iter()
			.find(|m| m.kind == "audio")
			.and_then(|m| m.formats.iter().find(|(_, f)| f.name.eq_ignore_ascii_case("opus")).map(|(pt, _)| *pt))
			.unwrap_or(OPUS_PAYLOAD_TYPE);

		let (video_tx, video_rx) = mpsc::unbounded_channel::<(RtpPacketInfo, LiveVideoCodec)>();
		let (audio_tx, audio_rx) = mpsc::unbounded_channel::<RtpPacketInfo>();
		let (presence_tx, presence_rx) = mpsc::unbounded_channel::<bool>();
		let (disconnected_tx, disconnected_rx) = watch::channel::<Option<String>>(None);

		{
			let video_tx = video_tx.clone();
			let audio_tx = audio_tx.clone();
			let video_payload_types = video_payload_types.clone();
			peer.on_track(Box::new(move |track, _receiver, _transceiver| {
				let video_tx = video_tx.clone();
				let audio_tx = audio_tx.clone();
				let video_payload_types = video_payload_types.clone();
				Box::pin(async move {
					let is_video = track.kind() == RTPCodecType::Video;
					while let Ok((packet, _attributes)) = track.read_rtp().await {
						let info = RtpPacketInfo {
							sequence_number: packet.header.sequence_number,
							timestamp: packet.header.timestamp,
							marker: packet.header.marker,
							ssrc: packet.header.ssrc,
							payload: packet.payload.to_vec(),
						};
						if is_video {
							if let Some(codec) = video_payload_types.get(&packet.header.payload_type) {
								let _ = video_tx.send((info, *codec));
							}
						} else if packet.header.payload_type == opus_payload_type {
							let _ = audio_tx.send(info);
						}
					}
				})
			}));
		}

		let disconnected_for_ice = disconnected_tx.clone();
		peer.on_ice_connection_state_change(Box::new(move |state: RTCIceConnectionState| {
			debug!("Broadcast ICE connection state {state:?}");
			let disconnected = disconnected_for_ice.clone();
			Box::pin(async move {
				if matches!(state, RTCIceConnectionState::Failed | RTCIceConnectionState::Closed) {
					let _ = disconnected.send(Some(format!("media connection {state:?}")));
				}
			})
		}));
		let disconnected_for_peer = disconnected_tx.clone();
		peer.on_peer_connection_state_change(Box::new(move |state: RTCPeerConnectionState| {
			debug!("Broadcast peer connection state {state:?}");
			let disconnected = disconnected_for_peer.clone();
			Box::pin(async move {
				if matches!(state, RTCPeerConnectionState::Failed | RTCPeerConnectionState::Closed) {
					let _ = disconnected.send(Some(format!("media connection {state:?}")));
				}
			})
		}));

		let mut last_error: Option<Box<dyn std::error::Error + Send + Sync>> = None;
		for edge in gateway.edges.iter().take(3) {
			match join_through_edge(
				&peer,
				&offer_sdp,
				credentials,
				&gateway,
				edge,
				&session_id,
				user_agent,
				&ortc,
				&disconnected_tx,
				&presence_tx,
			)
			.await
			{
				Ok(socket) => {
					return Ok(Self {
						peer,
						socket,
						video_rx,
						audio_rx,
						presence_rx,
						disconnected_rx,
					});
				}
				Err(e) => {
					warn!("Could not join the broadcast through gateway edge {}: {e}", edge.ip);
					last_error = Some(e);
				}
			}
		}
		Err(format!("Could not join the broadcast through any media gateway. Last error: {last_error:?}").into())
	}

	/// Asks the publisher for a new key frame (picture loss indication).
	pub async fn request_key_frame(&self, media_ssrc: u32) {
		Self::request_key_frame_on(&self.peer, media_ssrc).await;
	}

	/// Asks for retransmission of missing packets with generic NACKs.
	pub async fn request_retransmission(&self, media: MediaKind, media_ssrc: u32, sequence_numbers: &[u16]) {
		Self::request_retransmission_on(&self.peer, media, media_ssrc, sequence_numbers).await;
	}

	/// Key-frame request without a session, for the recorder's media pipeline.
	pub async fn request_key_frame_on(peer: &Arc<RTCPeerConnection>, media_ssrc: u32) {
		send_feedback_on(peer, &[Box::new(PictureLossIndication { sender_ssrc: 1, media_ssrc })]).await;
	}

	/// Retransmission request without a session, for the recorder's media pipeline.
	pub async fn request_retransmission_on(
		peer: &Arc<RTCPeerConnection>,
		media: MediaKind,
		media_ssrc: u32,
		sequence_numbers: &[u16],
	) {
		let _ = media;
		let mut packets: Vec<Box<dyn webrtc::rtcp::packet::Packet + Send + Sync>> = Vec::new();
		let mut index = 0;
		while index < sequence_numbers.len() {
			let first = sequence_numbers[index];
			index += 1;
			let mut mask = 0u16;
			while index < sequence_numbers.len() {
				let offset = sequence_numbers[index].wrapping_sub(first).wrapping_sub(1);
				if offset > 15 {
					break;
				}
				mask |= 1 << offset;
				index += 1;
			}
			packets.push(Box::new(TransportLayerNack {
				sender_ssrc: 1,
				media_ssrc,
				nacks: vec![NackPair { packet_id: first, lost_packets: mask }],
			}));
		}
		send_feedback_on(peer, &packets).await;
	}

	/// Resolves with the reason once the media or gateway connection is lost.
	pub async fn disconnected(&self) -> String {
		let mut rx = self.disconnected_rx.clone();
		loop {
			if let Some(reason) = rx.borrow().clone() {
				return reason;
			}
			if rx.changed().await.is_err() {
				return "closed".to_string();
			}
		}
	}

	pub async fn dispose(self) {
		self.socket.dispose().await;
		let _ = self.peer.close().await;
	}

	/// Disposes the parts after the receiving halves were moved out.
	pub async fn dispose_parts(peer: Arc<RTCPeerConnection>, socket: Arc<GatewaySocket>) {
		socket.dispose().await;
		let _ = peer.close().await;
	}
}

async fn send_feedback_on(peer: &Arc<RTCPeerConnection>, packets: &[Box<dyn webrtc::rtcp::packet::Packet + Send + Sync>]) {
	if let Err(e) = peer.write_rtcp(packets).await {
		debug!("Could not send RTCP feedback: {e}");
	}
}

#[allow(clippy::too_many_arguments)]
async fn join_through_edge(
	peer: &Arc<RTCPeerConnection>,
	offer: &protocol::OfferSdp,
	credentials: &AgoraCredentials,
	gateway: &AgoraGatewayInfo,
	edge: &AgoraEdge,
	session_id: &str,
	user_agent: &str,
	ortc: &Value,
	disconnected_tx: &watch::Sender<Option<String>>,
	presence_tx: &mpsc::UnboundedSender<bool>,
) -> Result<Arc<GatewaySocket>, Box<dyn std::error::Error + Send + Sync>> {
	let socket = GatewaySocket::connect(&edge.gateway_uri(), user_agent).await?;
	let join = protocol::build_join_message(
		credentials,
		gateway,
		edge,
		session_id,
		user_agent,
		ortc.clone(),
		chrono::Utc::now().timestamp_millis(),
		&format!("process-{:032x}", rand::random::<u128>()),
	);
	let joined = socket.request("join_v3", Some(join)).await?;
	let server_ortc = joined.get("ortc").cloned().ok_or("The media gateway did not return connection parameters.")?;
	let answer = protocol::build_answer_sdp(offer, &server_ortc);
	peer.set_remote_description(RTCSessionDescription::answer(answer)?).await?;

	socket.start_keep_alive();

	// Gateway events drive subscriptions and presence; socket loss ends the session.
	let socket = Arc::new(socket);
	let socket_for_close = socket.clone();
	let disconnected_for_close = disconnected_tx.clone();
	tokio::spawn(async move {
		let reason = socket_for_close.closed().await;
		let _ = disconnected_for_close.send(Some(reason));
	});
	let socket_for_events = socket.clone();
	let presence_tx = presence_tx.clone();
	let disconnected_for_events = disconnected_tx.clone();
	tokio::spawn(async move {
		let disconnected_tx = disconnected_for_events;
		let mut events = socket_for_events.take_events().expect("events taken once");
		let mut broadcaster_uid: Option<u64> = None;
		let mut subscriptions: std::collections::HashSet<String> = std::collections::HashSet::new();
		while let Some((event_type, message)) = events.recv().await {
			match event_type.as_str() {
				"on_add_video_stream" | "on_add_audio_stream" => {
					if let Some(uid) = message.get("uid").and_then(Value::as_u64) {
						broadcaster_uid = Some(uid);
					}
					let _ = presence_tx.send(false);
					let kind = if event_type == "on_add_video_stream" { "video" } else { "audio" };
					if let (Some(uid), Some(ssrc)) = (message.get("uid"), message.get("ssrcId"))
						&& subscriptions.insert(format!("{kind}:{uid}:{ssrc}"))
					{
							let subscribe = protocol::build_subscribe_message(uid, kind, ssrc);
							match tokio::time::timeout(
								std::time::Duration::from_secs(15),
								socket_for_events.request("subscribe", Some(subscribe)),
							)
							.await
							{
								Ok(Ok(_)) => debug!("Subscribed to the broadcast {kind} stream"),
								Ok(Err(e)) => {
									warn!("Subscribing to the broadcast {kind} stream failed: {e}");
									let _ = disconnected_tx.send(Some(format!("subscribe to {kind} failed")));
								}
								Err(_) => {
									warn!("Subscribing to the broadcast {kind} stream timed out");
									let _ = disconnected_tx.send(Some(format!("subscribe to {kind} failed")));
								}
							}
					}
				}
				"on_user_online" if message.get("uid").and_then(Value::as_u64) == broadcaster_uid => {
					let _ = presence_tx.send(false);
				}
				"on_user_offline" if message.get("uid").and_then(Value::as_u64) == broadcaster_uid => {
					info!("Broadcaster left the channel ({:?})", message.get("reason").and_then(Value::as_str));
					let _ = presence_tx.send(true);
				}
				"on_remove_stream" if message.get("uid").and_then(Value::as_u64) == broadcaster_uid => {
					info!("Broadcaster stopped publishing a stream");
				}
				"on_p2p_lost" => {
					let _ = disconnected_tx.send(Some("media path lost".to_string()));
				}
				"on_notification" if message.get("action").and_then(Value::as_str) == Some("quit") => {
					let code = message.get("code").and_then(Value::as_i64).unwrap_or(-1);
					let _ = disconnected_tx.send(Some(format!("gateway asked the viewer to quit (code {code})")));
				}
				_ => {}
			}
		}
	});
	Ok(socket)
}

/// Edge discovery: asks the media vendor for gateway edges. Talks to the
/// vendor, not the site, so it deliberately uses the plain unsigned client.
async fn choose_server(
	http: &reqwest::Client,
	credentials: &AgoraCredentials,
	session_id: &str,
) -> Result<AgoraGatewayInfo, Box<dyn std::error::Error + Send + Sync>> {
	let opid = (rand::random::<u64>() % 999_999_999_999) as i64 + 1;
	let request = protocol::build_choose_server_request(credentials, session_id, opid, chrono::Utc::now().timestamp_millis());
	let mut last_code = 0i64;
	for host in protocol::CHOOSE_SERVER_HOSTS {
		match try_host(http, host, &request).await {
			Ok(Some(gateway)) => return Ok(gateway),
			Ok(None) => {
				// A granted-but-failed gateway reports its code in the response.
				last_code = -1;
			}
			Err(e) => warn!("Media edge discovery failed at {host}: {e}"),
		}
	}
	Err(format!("No media gateway was granted for the broadcast (code {last_code}).").into())
}

async fn try_host(
	http: &reqwest::Client,
	host: &str,
	request: &serde_json::Value,
) -> Result<Option<AgoraGatewayInfo>, Box<dyn std::error::Error + Send + Sync>> {
	let form = reqwest::multipart::Form::new().text("request", request.to_string());
	let response = http.post(protocol::choose_server_url(host)).multipart(form).send().await?;
	if !response.status().is_success() {
		return Ok(None);
	}
	let body = response.text().await?;
	Ok(protocol::parse_choose_server_response(&body)?)
}

async fn create_peer() -> Result<Arc<RTCPeerConnection>, Box<dyn std::error::Error + Send + Sync>> {
	let mut media_engine = MediaEngine::default();
	let video_feedback: Vec<RTCPFeedback> = VIDEO_FEEDBACK
		.iter()
		.flat_map(|entry| match entry.split_once(' ') {
			Some((typ, parameter)) => vec![RTCPFeedback { typ: typ.to_string(), parameter: parameter.to_string() }],
			None => vec![RTCPFeedback { typ: entry.to_string(), parameter: String::new() }],
		})
		.collect();
	let audio_feedback: Vec<RTCPFeedback> = AUDIO_FEEDBACK
		.iter()
		.map(|typ| RTCPFeedback { typ: typ.to_string(), parameter: String::new() })
		.collect();
	media_engine.register_codec(
		RTCRtpCodecParameters {
			capability: RTCRtpCodecCapability {
				mime_type: "audio/opus".to_string(),
				clock_rate: 48_000,
				channels: 2,
				sdp_fmtp_line: "minptime=10;useinbandfec=1".to_string(),
				rtcp_feedback: audio_feedback,
			},
			payload_type: OPUS_PAYLOAD_TYPE,
			stats_id: String::new(),
		},
		RTPCodecType::Audio,
	)?;
	media_engine.register_codec(
		RTCRtpCodecParameters {
			capability: RTCRtpCodecCapability {
				mime_type: "video/VP8".to_string(),
				clock_rate: 90_000,
				channels: 0,
				sdp_fmtp_line: String::new(),
				rtcp_feedback: video_feedback.clone(),
			},
			payload_type: VP8_PAYLOAD_TYPE,
			stats_id: String::new(),
		},
		RTPCodecType::Video,
	)?;
	for (payload_type, profile_level_id) in [(H264_BASELINE_PAYLOAD_TYPE, "42e01f"), (H264_HIGH_PAYLOAD_TYPE, "640c1f")] {
		media_engine.register_codec(
			RTCRtpCodecParameters {
				capability: RTCRtpCodecCapability {
					mime_type: "video/H264".to_string(),
					clock_rate: 90_000,
					channels: 0,
					sdp_fmtp_line: format!("level-asymmetry-allowed=1;packetization-mode=1;profile-level-id={profile_level_id}"),
					rtcp_feedback: video_feedback.clone(),
				},
				payload_type,
				stats_id: String::new(),
			},
			RTPCodecType::Video,
		)?;
	}

	let api = APIBuilder::new().with_media_engine(media_engine).build();
	let peer = Arc::new(api.new_peer_connection(RTCConfiguration::default()).await?);
	peer.add_transceiver_from_kind(
		RTPCodecType::Audio,
		Some(RTCRtpTransceiverInit { direction: RTCRtpTransceiverDirection::Recvonly, send_encodings: vec![] }),
	)
	.await?;
	peer.add_transceiver_from_kind(
		RTPCodecType::Video,
		Some(RTCRtpTransceiverInit { direction: RTCRtpTransceiverDirection::Recvonly, send_encodings: vec![] }),
	)
	.await?;
	Ok(peer)
}
