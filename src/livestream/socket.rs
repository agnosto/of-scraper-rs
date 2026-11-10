//! The gateway's JSON-over-WebSocket channel: requests carry an `_id`
//! answered by a matching response, and unsolicited frames are events.
//! Ported from SubScraper's `AgoraGatewaySocket`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use log::{debug, warn};
use serde_json::{json, Value};
use tokio::sync::{mpsc, oneshot, watch};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
const PING_INTERVAL: Duration = Duration::from_secs(3);
const MAX_MISSED_PINGS: usize = 4;

#[derive(Debug, thiserror::Error)]
#[error("The media gateway rejected '{request}' (code {code})")]
pub struct GatewayError {
	pub request: String,
	pub code: i64,
}

type Pending = Arc<Mutex<HashMap<String, oneshot::Sender<Result<Value, GatewayError>>>>>;

/// One JSON-over-WebSocket connection to a media gateway edge.
pub struct GatewaySocket {
	send: mpsc::Sender<Message>,
	pending: Pending,
	/// Unsolicited gateway frames as (event type, message body). Taken once
	/// by the session's event loop.
	events: Mutex<Option<mpsc::UnboundedReceiver<(String, Value)>>>,
	/// `None` while open, `Some(reason)` once the connection is gone.
	closed: watch::Receiver<Option<String>>,
	lifetime: Arc<tokio::sync::Notify>,
	tasks: Mutex<Vec<tokio::task::JoinHandle<()>>>,
}

impl GatewaySocket {
	pub async fn connect(uri: &str, user_agent: &str) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
		let mut request = uri.into_client_request()?;
		request.headers_mut().insert(
			tokio_tungstenite::tungstenite::http::header::USER_AGENT,
			user_agent.parse()?,
		);
		let (ws, _response) = tokio_tungstenite::connect_async(request).await?;
		let (mut write, mut read) = ws.split();

		let (send_tx, mut send_rx) = mpsc::channel::<Message>(64);
		let (events_tx, events_rx) = mpsc::unbounded_channel::<(String, Value)>();
		let pending: Pending = Arc::new(Mutex::new(HashMap::new()));
		let (closed_tx, closed_rx) = watch::channel::<Option<String>>(None);
		let lifetime = Arc::new(tokio::sync::Notify::new());

		// Writer task: everything sent goes through this one task so frames stay whole and ordered.
		let writer_lifetime = lifetime.clone();
		let writer = tokio::spawn(async move {
			while let Some(message) = send_rx.recv().await {
				if write.send(message).await.is_err() {
					break;
				}
			}
			let _ = &writer_lifetime;
		});

		// Reader task: answers pending requests, forwards events, and on exit
		// fails every pending request and records the close reason.
		let reader_pending = pending.clone();
		let reader_events = events_tx.clone();
		let reader_closed = closed_tx.clone();
		let reader = tokio::spawn(async move {
			let mut reason = "connection closed".to_string();
			loop {
				match read.next().await {
					Some(Ok(Message::Text(text))) => dispatch(&text, &reader_pending, &reader_events),
					Some(Ok(Message::Close(frame))) => {
						reason = format!("gateway closed the connection ({:?})", frame.map(|f| f.reason));
						break;
					}
					Some(Ok(_)) => {}
					Some(Err(e)) => {
						reason = e.to_string();
						break;
					}
					None => break,
				}
			}
			for (_, pending) in reader_pending.lock().unwrap().drain() {
				let _ = pending.send(Err(GatewayError { request: "pending".into(), code: -1 }));
			}
			let _ = reader_closed.send(Some(reason));
		});

		Ok(Self {
			send: send_tx,
			pending,
			events: Mutex::new(Some(events_rx)),
			closed: closed_rx,
			lifetime,
			tasks: Mutex::new(vec![writer, reader]),
		})
	}

	/// Takes the event receiver; only the first call returns it.
	pub fn take_events(&self) -> Option<mpsc::UnboundedReceiver<(String, Value)>> {
		self.events.lock().unwrap().take()
	}

	/// Sends a request and returns the response's `_message`, failing when the gateway refuses it.
	pub async fn request(&self, request_type: &str, message: Option<Value>) -> Result<Value, GatewayError> {
		let id = format!("{:012x}", rand::random::<u64>() & 0xFF_FFFF_FFFF);
		let frame = json_frame(&id, request_type, message);
		let (tx, rx) = oneshot::channel();
		self.pending.lock().unwrap().insert(id.clone(), tx);
		if self.send.send(Message::Text(frame.to_string().into())).await.is_err() {
			self.pending.lock().unwrap().remove(&id);
			return Err(GatewayError { request: request_type.to_string(), code: -2 });
		}
		let response = match tokio::time::timeout(REQUEST_TIMEOUT, rx).await {
			Ok(Ok(result)) => result?,
			Ok(Err(_)) => return Err(GatewayError { request: request_type.to_string(), code: -3 }),
			Err(_) => {
				self.pending.lock().unwrap().remove(&id);
				return Err(GatewayError { request: request_type.to_string(), code: -4 });
			}
		};
		let body = response.get("_message").cloned().unwrap_or_else(|| json!({}));
		if response.get("_result").and_then(Value::as_str) != Some("success") {
			let code = body
				.get("error_code")
				.or_else(|| body.get("code"))
				.and_then(Value::as_i64)
				.unwrap_or(-1);
			return Err(GatewayError { request: request_type.to_string(), code });
		}
		Ok(body)
	}

	/// Keeps the connection alive with gateway pings; aborts after too many unanswered ones.
	pub fn start_keep_alive(&self) {
		let send = self.send.clone();
		let pending = self.pending.clone();
		let closed = self.closed.clone();
		let lifetime = self.lifetime.clone();
		let task = tokio::spawn(async move {
			let mut missed = 0;
			loop {
				tokio::time::sleep(PING_INTERVAL).await;
				if closed.borrow().is_some() {
					return;
				}
				let id = format!("{:012x}", rand::random::<u64>() & 0xFF_FFFF_FFFF);
				let frame = json_frame(&id, "ping", None);
				let (tx, rx) = oneshot::channel();
				pending.lock().unwrap().insert(id.clone(), tx);
				if send.send(Message::Text(frame.to_string().into())).await.is_err() {
					return;
				}
				match tokio::time::timeout(REQUEST_TIMEOUT, rx).await {
					Ok(Ok(Ok(_))) => missed = 0,
					Ok(Ok(Err(_))) | Ok(Err(_)) | Err(_) => {
						missed += 1;
						if missed >= MAX_MISSED_PINGS {
							warn!("Media gateway stopped answering pings.");
							lifetime.notify_waiters();
							// Dropping the sender closes the writer task and thus the socket.
							return;
						}
					}
				}
			}
		});
		self.tasks.lock().unwrap().push(task);
	}

	/// A future that resolves with the reason once the connection is gone.
	pub async fn closed(&self) -> String {
		let mut rx = self.closed.clone();
		loop {
			if let Some(reason) = rx.borrow().clone() {
				return reason;
			}
			if rx.changed().await.is_err() {
				return "closed".to_string();
			}
		}
	}

	pub async fn dispose(&self) {
		// The gateway removes viewers that disappear, so a failed leave is harmless.
		let _ = tokio::time::timeout(Duration::from_secs(3), self.request("leave", None)).await;
		for task in self.tasks.lock().unwrap().drain(..) {
			task.abort();
		}
	}
}

fn json_frame(id: &str, request_type: &str, message: Option<Value>) -> Value {
	let mut frame = json!({ "_id": id, "_type": request_type });
	if let Some(message) = message {
		frame["_message"] = message;
	}
	frame
}

fn dispatch(text: &str, pending: &Pending, events: &mpsc::UnboundedSender<(String, Value)>) {
	let Ok(frame) = serde_json::from_str::<Value>(text) else {
		debug!("Ignoring a non-JSON media gateway frame.");
		return;
	};
	if let Some(id) = frame.get("_id").and_then(Value::as_str)
		&& let Some(sender) = pending.lock().unwrap().remove(id)
	{
		let _ = sender.send(Ok(frame));
		return;
	}
	let Some(event_type) = frame.get("_type").and_then(Value::as_str) else {
		return;
	};
	let body = frame.get("_message").cloned().unwrap_or_else(|| json!({}));
	debug!("Media gateway event {event_type}");
	let _ = events.send((event_type.to_string(), body));
}
