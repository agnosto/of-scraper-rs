//! Message shapes for joining a broadcast through the media vendor's web
//! gateway as a receive-only viewer, matching what the site's own web player
//! sends. Ported from SubScraper's `AgoraProtocol`.

use serde_json::{json, Value};

use of_client::livestream::AgoraCredentials;

pub const SDK_VERSION: &str = "4.23.1";
pub const CODEC: &str = "vp8";
pub const MODE: &str = "live";
pub const ROLE: &str = "audience";

const GATEWAY_FLAG: i64 = 4096;

pub const CHOOSE_SERVER_HOSTS: [&str; 4] = [
	"webrtc2-ap-web-1.agora.io",
	"webrtc2-2.ap.sd-rtn.com",
	"webrtc2-ap-web-3.agora.io",
	"webrtc2-4.ap.sd-rtn.com",
];

pub fn choose_server_url(host: &str) -> String {
	format!("https://{host}/api/v2/transpond/webrtc?v=2")
}

#[derive(Debug, Clone)]
pub struct AgoraEdge {
	pub ip: String,
	pub port: u16,
	pub ticket: String,
}

impl AgoraEdge {
	/// The gateway WebSocket host: the edge IP with dots replaced by dashes.
	pub fn gateway_uri(&self) -> String {
		format!("wss://{}.edge.agora.io:{}", self.ip.replace('.', "-"), self.port)
	}
}

#[derive(Debug, Clone)]
pub struct AgoraGatewayInfo {
	pub ap_response: Value,
	pub edges: Vec<AgoraEdge>,
}

pub fn build_choose_server_request(credentials: &AgoraCredentials, session_id: &str, opid: i64, client_timestamp: i64) -> Value {
	json!({
		"appid": credentials.app_id,
		"client_ts": client_timestamp,
		"opid": opid,
		"sid": session_id,
		"request_bodies": [{
			"uri": 22,
			"buffer": {
				"cname": credentials.channel,
				// Area code for both services, and role 2 (audience).
				"detail": { "11": "GLOBAL", "17": "2", "22": "GLOBAL" },
				"key": credentials.token,
				"service_ids": [11, 26],
				"uid": credentials.user_id,
			}
		}]
	})
}

/// Reads the gateway edges from a choose-server response, or `None` when none were granted.
pub fn parse_choose_server_response(body: &str) -> Result<Option<AgoraGatewayInfo>, serde_json::Error> {
	let response: Value = serde_json::from_str(body)?;
	let gateway = response["response_body"]
		.as_array()
		.and_then(|items| {
			items.iter().find_map(|item| {
				let buffer = item.get("buffer")?;
				if buffer.get("flag").and_then(Value::as_i64) == Some(GATEWAY_FLAG) {
					Some(buffer)
				} else {
					None
				}
			})
		})
		.ok_or_else(|| serde_json::Error::io(std::io::Error::new(std::io::ErrorKind::InvalidData, "no gateway buffer")))?;

	let error_code = gateway.get("code").and_then(Value::as_i64).unwrap_or(0);
	let ticket = gateway.get("cert").and_then(Value::as_str).unwrap_or("").to_string();
	let mut edges = Vec::new();
	if let Some(edge_services) = gateway.get("edges_services").and_then(Value::as_array) {
		for edge in edge_services {
			if let (Some(ip), Some(port)) = (
				edge.get("ip").and_then(Value::as_str),
				edge.get("port").and_then(Value::as_i64),
			)
				&& !ip.trim().is_empty()
				&& port > 0
				&& port <= u16::MAX as i64
			{
				edges.push(AgoraEdge { ip: ip.to_string(), port: port as u16, ticket: ticket.clone() });
			}
		}
	}

	if error_code != 0 || edges.is_empty() {
		return Ok(None);
	}

	let mut detail = gateway
		.get("detail")
		.cloned()
		.unwrap_or_else(|| json!({}));
	if let Some(top_detail) = response.get("detail") {
		merge(&mut detail, top_detail);
	}

	let ap_response = json!({
		"code": error_code,
		"server_ts": response.get("enter_ts"),
		"uid": gateway.get("uid"),
		"cid": gateway.get("cid"),
		"cname": gateway.get("cname"),
		"detail": detail,
		"flag": GATEWAY_FLAG,
		"opid": response.get("opid"),
		"cert": ticket,
	});
	Ok(Some(AgoraGatewayInfo { ap_response, edges }))
}

fn merge(into: &mut Value, from: &Value) {
	if let (Some(into_obj), Some(from_obj)) = (into.as_object_mut(), from.as_object()) {
		for (key, value) in from_obj {
			into_obj.insert(key.clone(), value.clone());
		}
	}
}

// --- Minimal SDP parsing of the local offer ---------------------------------

#[derive(Debug, Clone)]
pub struct MediaFormat {
	pub name: String,
	pub clock_rate: u32,
	pub channels: u32,
	pub fmtp: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct MediaLine {
	pub kind: String,
	pub media_id: String,
	pub payload_types: Vec<u8>,
	pub formats: std::collections::BTreeMap<u8, MediaFormat>,
	pub ice_ufrag: Option<String>,
	pub ice_pwd: Option<String>,
	pub fingerprint: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct OfferSdp {
	pub media: Vec<MediaLine>,
	pub ice_ufrag: Option<String>,
	pub ice_pwd: Option<String>,
	pub dtls_fingerprint: Option<String>,
}

/// Parses the parts of an SDP offer that the gateway join needs: per-m-line
/// payload types/formats, ICE credentials, DTLS fingerprint and mids.
pub fn parse_offer(sdp: &str) -> OfferSdp {
	let mut offer = OfferSdp::default();
	let mut current: Option<usize> = None;
	for line in sdp.lines() {
		let line = line.trim_end_matches('\r');
		if let Some(rest) = line.strip_prefix("m=") {
			let mut parts = rest.split_whitespace();
			let kind = parts.next().unwrap_or("").to_string();
			let payload_types = parts.filter_map(|p| p.parse::<u8>().ok()).collect();
			offer.media.push(MediaLine { kind, payload_types, ..Default::default() });
			current = Some(offer.media.len() - 1);
			continue;
		}
		let Some(attr) = line.strip_prefix("a=") else { continue };
		let (name, value) = attr.split_once(':').unwrap_or((attr, ""));
		let media = current.and_then(|i| offer.media.get_mut(i));
		match name {
			"mid" => {
				if let Some(m) = media {
					m.media_id = value.to_string();
				}
			}
			"ice-ufrag" => set_media_or_session(media.map(|m| &mut m.ice_ufrag), &mut offer.ice_ufrag, value),
			"ice-pwd" => set_media_or_session(media.map(|m| &mut m.ice_pwd), &mut offer.ice_pwd, value),
			"fingerprint" => {
				set_media_or_session(media.map(|m| &mut m.fingerprint), &mut offer.dtls_fingerprint, value);
			}
			"rtpmap" => {
				if let Some(m) = media
					&& let Some((pt, desc)) = value.split_once(char::is_whitespace)
					&& let Ok(pt) = pt.parse::<u8>()
				{
					let mut desc_parts = desc.splitn(3, '/');
					let name = desc_parts.next().unwrap_or("").to_string();
					let clock_rate = desc_parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);
					let channels = desc_parts.next().and_then(|s| s.parse().ok()).unwrap_or(1);
					m.formats.insert(pt, MediaFormat { name, clock_rate, channels, fmtp: None });
				}
			}
			"fmtp" => {
				if let Some(m) = media
					&& let Some((pt, fmtp)) = value.split_once(char::is_whitespace)
					&& let Ok(pt) = pt.parse::<u8>()
					&& let Some(format) = m.formats.get_mut(&pt)
				{
					format.fmtp = Some(fmtp.to_string());
				}
			}
			_ => {}
		}
	}
	offer
}

/// Sets a media-level attribute when inside an m-line, otherwise the session-level one.
fn set_media_or_session(media_slot: Option<&mut Option<String>>, session_slot: &mut Option<String>, value: &str) {
	match media_slot {
		Some(slot) => *slot = Some(value.to_string()),
		None => *session_slot = Some(value.to_string()),
	}
}

fn codec_entries(media: &MediaLine, names: &[&str], feedback: &[&str]) -> Value {
	let codecs: Vec<Value> = media
		.formats
		.iter()
		.filter(|(_, format)| names.iter().any(|n| format.name.eq_ignore_ascii_case(n)))
		.map(|(payload_type, format)| {
			let payload_type = *payload_type;
			// The gateway matches browser spellings, which are lower case for opus.
			let name = if format.name.eq_ignore_ascii_case("opus") {
				"opus".to_string()
			} else {
				format.name.clone()
			};
			let mut rtp_map = json!({
				"encodingName": name,
				"clockRate": format.clock_rate,
			});
			if format.channels > 1 {
				rtp_map["encodingParameters"] = json!(format.channels);
			}
			let mut parameters = serde_json::Map::new();
			if let Some(fmtp) = &format.fmtp {
				for pair in fmtp.split(';').filter(|p| !p.is_empty()) {
					if let Some((key, value)) = pair.split_once('=') {
						parameters.insert(key.trim().to_string(), json!(value.trim()));
					} else {
						parameters.insert(pair.trim().to_string(), json!(""));
					}
				}
			}
			json!({
				"payloadType": payload_type,
				"rtpMap": rtp_map,
				"rtcpFeedbacks": feedback.iter().map(|entry| {
					match entry.split_once(' ') {
						Some((typ, parameter)) => json!({ "type": typ, "parameter": parameter }),
						None => json!({ "type": entry }),
					}
				}).collect::<Vec<_>>(),
				"fmtp": { "parameters": Value::Object(parameters) },
			})
		})
		.collect();
	Value::Array(codecs)
}

fn empty_capabilities() -> Value {
	json!({
		"audioCodecs": [],
		"audioExtensions": [],
		"videoCodecs": [],
		"videoExtensions": [],
	})
}

/// Describes the local receive-only peer in the gateway's ORTC form.
pub fn build_local_ortc(offer: &OfferSdp) -> Result<Value, String> {
	let audio = offer.media.iter().find(|m| m.kind == "audio").ok_or("the local offer has no audio m-line")?;
	let video = offer.media.iter().find(|m| m.kind == "video").ok_or("the local offer has no video m-line")?;
	let fingerprint = audio
		.fingerprint
		.clone()
		.or_else(|| offer.dtls_fingerprint.clone())
		.ok_or("the local offer has no DTLS fingerprint")?;
	let (hash_function, fingerprint_value) = fingerprint
		.split_once(' ')
		.ok_or("the local offer fingerprint is malformed")?;

	Ok(json!({
		"iceParameters": {
			"iceUfrag": audio.ice_ufrag.clone().or_else(|| offer.ice_ufrag.clone()),
			"icePwd": audio.ice_pwd.clone().or_else(|| offer.ice_pwd.clone()),
		},
		"dtlsParameters": {
			"fingerprints": [{
				"hashFunction": hash_function,
				"fingerprint": fingerprint_value,
			}]
		},
		"rtpCapabilities": {
			"send": empty_capabilities(),
			"recv": empty_capabilities(),
			"sendrecv": {
				"audioCodecs": codec_entries(audio, &["opus"], &["transport-cc", "nack"]),
				"audioExtensions": [],
				"videoCodecs": codec_entries(video, &["vp8", "h264"], &["transport-cc", "nack", "nack pli", "ccm fir"]),
				"videoExtensions": [],
			}
		},
		"version": "2",
	}))
}

#[allow(clippy::too_many_arguments)]
pub fn build_join_message(
	credentials: &AgoraCredentials,
	gateway: &AgoraGatewayInfo,
	edge: &AgoraEdge,
	session_id: &str,
	user_agent: &str,
	ortc: Value,
	join_timestamp: i64,
	process_id: &str,
) -> Value {
	let mut ap_response = gateway.ap_response.clone();
	ap_response["ticket"] = json!(edge.ticket);
	json!({
		"p2p_id": 1,
		"session_id": session_id,
		"app_id": credentials.app_id,
		"channel_key": credentials.token,
		"channel_name": credentials.channel,
		"sdk_version": SDK_VERSION,
		"browser": user_agent,
		"process_id": process_id,
		"mode": MODE,
		"codec": CODEC,
		"role": ROLE,
		"has_changed_gateway": false,
		"ap_response": ap_response,
		"extend": "",
		"details": {},
		"features": { "rejoin": true },
		"attributes": {
			"userAttributes": { "enablePublishedUserList": true, "maxSubscription": 50 }
		},
		"join_ts": join_timestamp,
		"ortc": ortc,
	})
}

pub fn build_subscribe_message(stream_id: &Value, stream_type: &str, ssrc_id: &Value) -> Value {
	json!({
		"stream_id": stream_id,
		"stream_type": stream_type,
		"mode": MODE,
		"codec": CODEC,
		"p2p_id": 1,
		"twcc": false,
		"rtx": false,
		"extend": "",
		"ssrcId": ssrc_id,
	})
}

/// Builds the remote answer from the gateway's ORTC parameters. An empty
/// codec list from the gateway means "use the offered opus/vp8 entry", which
/// is how the web player treats it.
pub fn build_answer_sdp(offer: &OfferSdp, server_ortc: &Value) -> String {
	let capabilities = server_ortc.get("rtpCapabilities").cloned().unwrap_or_else(|| json!({}));
	let send_capabilities = capabilities
		.get("sendrecv")
		.or_else(|| capabilities.get("send"))
		.cloned()
		.unwrap_or_else(|| json!({}));
	let ice = server_ortc.get("iceParameters");
	let fingerprint = server_ortc
		.get("dtlsParameters")
		.and_then(|d| d.get("fingerprints"))
		.and_then(Value::as_array)
		.and_then(|f| f.first())
		.cloned();
	let setup = match server_ortc
		.get("dtlsParameters")
		.and_then(|d| d.get("role"))
		.and_then(Value::as_str)
	{
		Some("server") => "passive",
		_ => "active",
	};

	let mut sdp = String::from("v=0\r\no=- 0 0 IN IP4 127.0.0.1\r\ns=AgoraGateway\r\nt=0 0\r\n");
	let mids: Vec<&str> = offer.media.iter().map(|m| m.media_id.as_str()).collect();
	sdp.push_str(&format!("a=group:BUNDLE {}\r\n", mids.join(" ")));
	sdp.push_str("a=ice-lite\r\na=msid-semantic: WMS\r\n");
	for media in &offer.media {
		let kind = if media.kind == "audio" { "audio" } else { "video" };
		let codecs_key = format!("{kind}Codecs");
		let (codec_lines, payload_types) = answer_codec_lines(
			send_capabilities.get(codecs_key).and_then(Value::as_array),
			media,
			if kind == "audio" { "opus" } else { "vp8" },
		);
		sdp.push_str(&format!("m={kind} 9 UDP/TLS/RTP/SAVPF {}\r\n", payload_types.iter().map(|p| p.to_string()).collect::<Vec<_>>().join(" ")));
		sdp.push_str("c=IN IP4 127.0.0.1\r\na=rtcp:9 IN IP4 0.0.0.0\r\n");
		sdp.push_str(&format!("a=ice-ufrag:{}\r\n", ice.and_then(|i| i.get("iceUfrag")).and_then(Value::as_str).unwrap_or("")));
		sdp.push_str(&format!("a=ice-pwd:{}\r\n", ice.and_then(|i| i.get("icePwd")).and_then(Value::as_str).unwrap_or("")));
		let (algorithm, fingerprint_value) = fingerprint
			.as_ref()
			.map(|f| {
				(
					f.get("algorithm").or_else(|| f.get("hashFunction")).and_then(Value::as_str).unwrap_or(""),
					f.get("fingerprint").and_then(Value::as_str).unwrap_or(""),
				)
			})
			.unwrap_or(("", ""));
		sdp.push_str(&format!("a=fingerprint:{algorithm} {fingerprint_value}\r\n"));
		sdp.push_str(&format!("a=setup:{setup}\r\na=mid:{}\r\n", media.media_id));
		if let Some(candidates) = ice.and_then(|i| i.get("candidates")).and_then(Value::as_array) {
			for candidate in candidates {
				sdp.push_str(&format!(
					"a=candidate:{} 1 {} {} {} {} typ {}\r\n",
					candidate.get("foundation").and_then(Value::as_str).unwrap_or(""),
					candidate.get("protocol").and_then(Value::as_str).unwrap_or(""),
					candidate.get("priority").and_then(Value::as_i64).unwrap_or(0),
					candidate.get("ip").and_then(Value::as_str).unwrap_or(""),
					candidate.get("port").and_then(Value::as_i64).unwrap_or(0),
					candidate.get("type").and_then(Value::as_str).unwrap_or(""),
				));
			}
		}
		sdp.push_str("a=sendonly\r\na=rtcp-mux\r\na=rtcp-rsize\r\n");
		for line in codec_lines {
			sdp.push_str(&line);
			sdp.push_str("\r\n");
		}
	}
	sdp
}

fn answer_codec_lines(server_codecs: Option<&Vec<Value>>, offered: &MediaLine, fallback_codec: &str) -> (Vec<String>, Vec<u8>) {
	let mut lines = Vec::new();
	let mut payload_types = Vec::new();
	if let Some(server_codecs) = server_codecs.filter(|c| !c.is_empty()) {
		for codec in server_codecs {
			let Some(payload_type) = codec.get("payloadType").and_then(Value::as_i64) else { continue };
			let Ok(payload_type) = u8::try_from(payload_type) else { continue };
			let Some(format) = offered.formats.get(&payload_type) else { continue };
			payload_types.push(payload_type);
			lines.push(rtp_map(payload_type, format));
			if let Some(feedbacks) = codec.get("rtcpFeedbacks").and_then(Value::as_array) {
				for feedback in feedbacks {
					let typ = feedback.get("type").and_then(Value::as_str).unwrap_or("");
					match feedback.get("parameter").and_then(Value::as_str) {
						Some(parameter) => lines.push(format!("a=rtcp-fb:{payload_type} {typ} {parameter}")),
						None => lines.push(format!("a=rtcp-fb:{payload_type} {typ}")),
					}
				}
			}
			if let Some(parameters) = codec.get("fmtp").and_then(|f| f.get("parameters")).and_then(Value::as_object) {
				if !parameters.is_empty() {
					let fmtp: Vec<String> = parameters.iter().map(|(k, v)| format!("{k}={}", v.as_str().unwrap_or(""))).collect();
					lines.push(format!("a=fmtp:{payload_type} {}", fmtp.join(";")));
				}
			} else if let Some(fmtp) = &format.fmtp
				&& !fmtp.is_empty()
			{
				lines.push(format!("a=fmtp:{payload_type} {fmtp}"));
			}
		}
	}
	if payload_types.is_empty() {
		for (pt, format) in &offered.formats {
			if format.name.eq_ignore_ascii_case(fallback_codec) {
				payload_types.push(*pt);
				lines.push(rtp_map(*pt, format));
				if let Some(fmtp) = &format.fmtp
					&& !fmtp.is_empty()
				{
					lines.push(format!("a=fmtp:{pt} {fmtp}"));
				}
			}
		}
	}
	(lines, payload_types)
}

fn rtp_map(payload_type: u8, format: &MediaFormat) -> String {
	let channels = if format.channels > 1 { format!("/{}", format.channels) } else { String::new() };
	format!("a=rtpmap:{payload_type} {}/{}{channels}", format.name, format.clock_rate)
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn choose_server_response_parses_edges() {
		let body = r#"{
			"enter_ts": 123, "opid": 456,
			"response_body": [{
				"uri": 22,
				"buffer": {
					"flag": 4096, "code": 0, "cert": "ticket", "uid": 7, "cid": "c", "cname": "chan",
					"detail": {"a": "b"},
					"edges_services": [
						{"ip": "1.2.3.4", "port": 443},
						{"ip": "", "port": 0}
					]
				}
			}],
			"detail": {"x": "y"}
		}"#;
		let info = parse_choose_server_response(body).unwrap().unwrap();
		assert_eq!(info.edges.len(), 1);
		assert_eq!(info.edges[0].gateway_uri(), "wss://1-2-3-4.edge.agora.io:443");
		assert_eq!(info.ap_response["cert"], json!("ticket"));
		assert_eq!(info.ap_response["detail"]["x"], json!("y"));
	}

	#[test]
	fn choose_server_response_without_gateway_is_none() {
		let body = r#"{"response_body": [{"buffer": {"flag": 4096, "code": 1, "edges_services": []}}]}"#;
		assert!(parse_choose_server_response(body).unwrap().is_none());
	}

	#[test]
	fn offer_parsing_reads_formats_and_ice() {
		let sdp = "v=0\r\no=- 1 1 IN IP4 127.0.0.1\r\ns=-\r\nt=0 0\r\n\
			a=fingerprint:sha-256 AA:BB\r\na=ice-ufrag:ufrag\r\na=ice-pwd:pwd\r\n\
			m=audio 9 UDP/TLS/RTP/SAVPF 111\r\na=mid:0\r\na=rtpmap:111 opus/48000/2\r\na=fmtp:111 minptime=10;useinbandfec=1\r\n\
			m=video 9 UDP/TLS/RTP/SAVPF 96 102\r\na=mid:1\r\na=rtpmap:96 VP8/90000\r\na=rtpmap:102 H264/90000\r\n";
		let offer = parse_offer(sdp);
		assert_eq!(offer.media.len(), 2);
		assert_eq!(offer.media[0].formats[&111].name, "opus");
		assert_eq!(offer.media[0].formats[&111].channels, 2);
		assert_eq!(offer.media[1].formats[&102].name, "H264");
		assert_eq!(offer.dtls_fingerprint.as_deref(), Some("sha-256 AA:BB"));
		let ortc = build_local_ortc(&offer).unwrap();
		assert_eq!(ortc["iceParameters"]["iceUfrag"], json!("ufrag"));
		assert_eq!(ortc["rtpCapabilities"]["sendrecv"]["audioCodecs"][0]["rtpMap"]["encodingName"], json!("opus"));
		assert_eq!(ortc["rtpCapabilities"]["sendrecv"]["videoCodecs"].as_array().unwrap().len(), 2);
	}

	#[test]
	fn answer_sdp_uses_server_codecs_and_falls_back() {
		let sdp = "v=0\r\nm=audio 9 UDP/TLS/RTP/SAVPF 111\r\na=mid:0\r\na=rtpmap:111 opus/48000/2\r\n\
			m=video 9 UDP/TLS/RTP/SAVPF 96\r\na=mid:1\r\na=rtpmap:96 VP8/90000\r\n";
		let offer = parse_offer(sdp);
		// Empty server codec list -> fall back to the offered opus/vp8 entries.
		let server_ortc = json!({
			"iceParameters": {"iceUfrag": "sufrag", "icePwd": "spwd", "candidates": [{"foundation": "f", "protocol": "udp", "priority": 1, "ip": "9.9.9.9", "port": 9, "type": "host"}]},
			"dtlsParameters": {"role": "server", "fingerprints": [{"hashFunction": "sha-256", "fingerprint": "FF"}]},
			"rtpCapabilities": {"sendrecv": {"audioCodecs": [], "videoCodecs": []}},
		});
		let answer = build_answer_sdp(&offer, &server_ortc);
		assert!(answer.contains("a=ice-lite\r\n"));
		assert!(answer.contains("a=ice-ufrag:sufrag\r\n"));
		assert!(answer.contains("a=setup:passive\r\n"));
		assert!(answer.contains("a=candidate:f 1 udp 1 9.9.9.9 9 typ host\r\n"));
		assert!(answer.contains("a=rtpmap:111 opus/48000/2\r\n"));
		assert!(answer.contains("a=rtpmap:96 VP8/90000\r\n"));
		assert!(answer.contains("m=video 9 UDP/TLS/RTP/SAVPF 96\r\n"));
	}
}
