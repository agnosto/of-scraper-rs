#![allow(dead_code)]
#![allow(async_fn_in_trait)]

use deserializers::from_str;
use crate::{media, user::User, OFClient};
use std::{slice, fmt};
use futures_util::TryFutureExt;
use reqwest::IntoUrl;
use serde::Deserialize;
use serde_json;
use chrono::{DateTime, Utc};
use log::*;

#[derive(Clone, Copy)]
pub enum ContentType {
	Posts,
	Chats,
	Stories,
	Notifications,
	Streams
}

impl fmt::Display for ContentType {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.write_str( match self {
			ContentType::Posts => "Posts",
			ContentType::Chats => "Messages",
			ContentType::Stories => "Stories",
			ContentType::Notifications => "Notifications",
			ContentType::Streams => "Streams",
		})
	}
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Post {
	pub id: u64,
	#[serde(default)]
	pub text: String,
	pub price: Option<f32>,
	pub author: User,
	#[serde(default)]
	can_toggle_favorite: bool,
	#[serde(default = "Utc::now")]
	pub posted_at: DateTime<Utc>,
	#[serde(default)]
	pub media: Vec<media::Feed>,
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Chat {
	pub id: u64,
	#[serde(default)]
	pub text: String,
	pub price: Option<f32>,
	#[serde(default)]
	pub is_free: bool,
	#[serde(default)]
	pub from_user: Option<ChatFromUser>,
	#[serde(default = "Utc::now")]
	pub created_at: DateTime<Utc>,
	#[serde(default)]
	pub media: Vec<media::Feed>,
}

#[derive(Deserialize, Debug, Clone)]
pub struct ChatFromUser {
	pub id: u64,
}

/// The real `/chats/{id}/messages` endpoint wraps results in an envelope
/// (`{"list": [...], "hasMore": bool, ...}`), it's not a bare JSON array —
/// see the captured response: `get_chats` was deserializing straight into
/// `Vec<Chat>` and failing to decode every single page.
#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
struct ChatsResponse {
	#[serde(default)]
	list: Vec<Chat>,
	#[serde(default)]
	has_more: bool,
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Story {
	pub id: u64,
	#[serde(default)]
	pub can_like: bool,
	#[serde(default = "Utc::now")]
	pub created_at: DateTime<Utc>,
	#[serde(default)]
	pub media: Vec<media::Feed>,
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Notification {
	#[serde(deserialize_with = "from_str")]
	pub id: u64,
	pub text: String,
	#[serde(default = "Utc::now")]
	pub created_at: DateTime<Utc>,
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Stream {
	pub id: u64,
	pub description: String,
	room: String,
	#[serde(default = "Utc::now")]
	pub started_at: DateTime<Utc>,
	#[serde(flatten)]
	pub media: media::Stream,
}

pub trait Content {
	fn id(&self) -> u64;
	fn timestamp(&self) -> DateTime<Utc>; 
	fn content_type() -> ContentType;
}

pub trait CanLike {
	fn can_like(&self) -> bool;
	fn like_url(&self) -> impl IntoUrl;
}

pub trait HasMedia {
	type Media: media::Media + Sync + Send;
	fn media(&self) -> &[Self::Media];
}

impl Content for Post {
	fn timestamp(&self) -> DateTime<Utc> { self.posted_at }
	fn id(&self) -> u64 { self.id }
	fn content_type() -> ContentType { ContentType::Posts }
}

impl CanLike for Post {
	fn can_like(&self) -> bool { self.can_toggle_favorite }
	fn like_url(&self) -> impl IntoUrl { format!("https://onlyfans.com/api2/v2/posts/{}/favorites/{}", self.id, self.author.id) }
}

impl HasMedia for Post {
	type Media = media::Feed;
	fn media(&self) -> &[Self::Media] { &self.media }
}

impl Content for Chat {
	fn id(&self) -> u64 { self.id }
	fn timestamp(&self) -> DateTime<Utc> { self.created_at }
	fn content_type() -> ContentType { ContentType::Chats }
}

impl CanLike for Chat {
	fn can_like(&self) -> bool { true }
	fn like_url(&self) -> impl IntoUrl { format!("https://onlyfans.com/api2/v2/messages/{}/like", self.id) }
}

impl HasMedia for Chat {
	type Media = media::Feed;
	fn media(&self) -> &[Self::Media] { &self.media }
}

impl Content for Story {
	fn id(&self) -> u64 { self.id }
	fn timestamp(&self) -> DateTime<Utc> { self.created_at }
	fn content_type() -> ContentType { ContentType::Stories }
}

impl CanLike for Story {
	fn can_like(&self) -> bool { self.can_like }
	fn like_url(&self) -> impl IntoUrl { format!("https://onlyfans.com/api2/v2/stories/{}/like", self.id) }
}

impl HasMedia for Story {
	type Media = media::Feed;
	fn media(&self) -> &[Self::Media] { &self.media }
}

impl Content for Notification {
	fn id(&self) -> u64 { self.id }
	fn timestamp(&self) -> DateTime<Utc> { self.created_at }
	fn content_type() -> ContentType { ContentType::Notifications }
}

impl Content for Stream {
	fn id(&self) -> u64 { self.id }
	fn timestamp(&self) -> DateTime<Utc> { self.started_at }
	fn content_type() -> ContentType { ContentType::Streams }
}

impl HasMedia for Stream {
	type Media = media::Stream;
	fn media(&self) -> &[Self::Media] { slice::from_ref(&self.media) }
}

impl OFClient {
	pub async fn get_post(&self, post_id: u64) -> reqwest_middleware::Result<Post> {
		self.get(format!("https://onlyfans.com/api2/v2/posts/{post_id}"))
		.send()
		.and_then(|response| response.json::<Post>().map_err(Into::into))
		.await
		.inspect(|content| info!("Got content: {:?}", content))
		.inspect_err(|err| error!("Error reading content {post_id}: {err:?}"))
	}

	pub async fn get_posts<I: fmt::Display>(&self, user_id: I, before_publish_time: Option<DateTime<Utc>>) -> reqwest_middleware::Result<Vec<Post>> {
		let mut url = format!("https://onlyfans.com/api2/v2/users/{user_id}/posts?limit=10");
		if let Some(time) = before_publish_time {
			url.push_str(&format!("&beforePublishTime={}.000000", time.timestamp()));
		}
		self.get(url)
		.send()
		.and_then(|response| response.json::<Vec<Post>>().map_err(Into::into))
		.await
	}

	pub async fn get_chats<I: fmt::Display>(&self, user_id: I, before_id: Option<u64>) -> reqwest_middleware::Result<Vec<Chat>> {
		let mut url = format!("https://onlyfans.com/api2/v2/chats/{user_id}/messages?limit=10&order=desc&skip_users=all");
		if let Some(id) = before_id {
			url.push_str(&format!("&id={}", id));
		}

		let response = self.get(url).send().await?;
		let body = response.text().await.map_err(reqwest_middleware::Error::Reqwest)?;

		match serde_json::from_str::<ChatsResponse>(&body) {
			Ok(wrapped) => Ok(wrapped.list),
			Err(e) => {
				let snippet: String = body.chars().take(2000).collect();
				error!("Failed to decode chats response ({}): {}", e, snippet);
				Err(reqwest_middleware::Error::Middleware(
					anyhow::anyhow!("Failed to decode chats response: {e}")
				))
			}
		}
	}

	pub async fn get_stories<I: fmt::Display>(&self, user_id: I) -> reqwest_middleware::Result<Vec<Story>> {
		let url = format!("https://onlyfans.com/api2/v2/users/{user_id}/stories?limit=100");
		self.get(url)
		.send()
		.and_then(|response| response.json::<Vec<Story>>().map_err(Into::into))
		.await
	}
}
