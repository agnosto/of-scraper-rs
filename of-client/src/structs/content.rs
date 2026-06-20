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
	Streams,
	Purchased,
	Highlights,
	Labels,
}

impl fmt::Display for ContentType {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.write_str( match self {
			ContentType::Posts => "Posts",
			ContentType::Chats => "Messages",
			ContentType::Stories => "Stories",
			ContentType::Notifications => "Notifications",
			ContentType::Streams => "Streams",
			ContentType::Purchased => "Purchases",
			ContentType::Highlights => "Highlights",
			ContentType::Labels => "Labels",
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

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
struct ChatsResponse {
	#[serde(default)]
	list: Vec<Chat>,
	#[serde(default)]
	has_more: bool,
}

#[derive(Deserialize, Debug)]
#[serde(tag = "responseType")]
pub enum Purchase {
	#[serde(rename = "message")]
	Message(Chat),
	#[serde(rename = "post")]
	Post(Post),
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
struct PurchasesResponse {
	#[serde(default)]
	list: Vec<Purchase>,
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

#[derive(Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct HighlightSummary {
	pub id: u64,
	#[serde(default)]
	pub title: String,
	#[serde(default)]
	pub stories_count: u32,
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
struct HighlightsResponse {
	#[serde(default)]
	list: Vec<HighlightSummary>,
	#[serde(default)]
	has_more: bool,
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
struct HighlightDetail {
	pub id: u64,
	#[serde(default)]
	pub title: String,
	#[serde(default = "Utc::now")]
	pub created_at: DateTime<Utc>,
	#[serde(default)]
	pub stories: Vec<Story>,
}

#[derive(Debug)]
pub struct Highlight {
	pub id: u64,
	pub title: String,
	pub created_at: DateTime<Utc>,
	media: Vec<media::Feed>,
}

#[derive(Deserialize, Debug, Clone)]
#[serde(untagged)]
pub enum LabelId {
	Number(u64),
	Text(String),
}

impl fmt::Display for LabelId {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		match self {
			LabelId::Number(n) => write!(f, "{}", n),
			LabelId::Text(s) => write!(f, "{}", s),
		}
	}
}

#[derive(Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct Label {
	pub id: LabelId,
	pub name: String,
	#[serde(rename = "type")]
	pub label_type: String,
	#[serde(default)]
	pub posts_count: u32,
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
struct LabelsResponse {
	#[serde(default)]
	list: Vec<Label>,
	#[serde(default)]
	has_more: bool,
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
struct LabelPostsResponse {
	#[serde(default)]
	list: Vec<Post>,
	#[serde(default)]
	has_more: bool,
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
	/// Used to build DRM license URLs, which need "message" or "post" in
	/// the path depending on what the content actually is. Matters for
	/// `Purchase`, which mixes both under one `content_type()`.
	fn drm_type_str(&self) -> &'static str { "post" }
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
	fn drm_type_str(&self) -> &'static str { "message" }
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

impl Content for Highlight {
	fn id(&self) -> u64 { self.id }
	fn timestamp(&self) -> DateTime<Utc> { self.created_at }
	fn content_type() -> ContentType { ContentType::Highlights }
}

impl HasMedia for Highlight {
	type Media = media::Feed;
	fn media(&self) -> &[Self::Media] { &self.media }
}

impl Content for Purchase {
	fn id(&self) -> u64 {
		match self {
			Purchase::Message(c) => c.id(),
			Purchase::Post(p) => p.id(),
		}
	}
	fn timestamp(&self) -> DateTime<Utc> {
		match self {
			Purchase::Message(c) => c.timestamp(),
			Purchase::Post(p) => p.timestamp(),
		}
	}
	fn content_type() -> ContentType { ContentType::Purchased }
	fn drm_type_str(&self) -> &'static str {
		match self {
			Purchase::Message(c) => c.drm_type_str(),
			Purchase::Post(p) => p.drm_type_str(),
		}
	}
}

impl HasMedia for Purchase {
	type Media = media::Feed;
	fn media(&self) -> &[Self::Media] {
		match self {
			Purchase::Message(c) => c.media(),
			Purchase::Post(p) => p.media(),
		}
	}
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

	/// Mirrors what the web client actually sends when scrolling up through
	/// a chat's history: `order=desc&skip_users=all` plus an `id` cursor set
	/// to the last message id seen so far (NOT a `beforeId` param, and not
	/// `beforeId` semantics either — it's literally `id=<that message's id>`
	/// and the API returns messages older than it).
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
				// Dump enough of the raw body to see what actually broke —
				// "error decoding response body" alone tells us nothing.
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

	/// Purchased content (PPV unlocks: paid posts AND paid chat messages,
	/// mixed together) for one specific creator. Unlike posts/chats, this
	/// endpoint paginates with a plain `offset` rather than a cursor id —
	/// the response itself tells you whether there's more via `hasMore`,
	/// which is why this returns it instead of inferring from page size.
	pub async fn get_purchased_content<I: fmt::Display>(&self, author: I, offset: u64) -> reqwest_middleware::Result<(Vec<Purchase>, bool)> {
		let url = format!(
			"https://onlyfans.com/api2/v2/posts/paid/all?limit=10&skip_users=all&format=infinite&offset={offset}&author={author}"
		);

		let response = self.get(url).send().await?;
		let body = response.text().await.map_err(reqwest_middleware::Error::Reqwest)?;

		match serde_json::from_str::<PurchasesResponse>(&body) {
			Ok(wrapped) => Ok((wrapped.list, wrapped.has_more)),
			Err(e) => {
				let snippet: String = body.chars().take(2000).collect();
				error!("Failed to decode purchases response ({}): {}", e, snippet);
				Err(reqwest_middleware::Error::Middleware(
					anyhow::anyhow!("Failed to decode purchases response: {e}")
				))
			}
		}
	}

	/// Lightweight list of a creator's highlight reels. Each entry here is
	/// just a cover + title — call `get_highlight` per id to actually get
	/// downloadable media.
	pub async fn get_highlights<I: fmt::Display>(&self, user_id: I, offset: u64) -> reqwest_middleware::Result<(Vec<HighlightSummary>, bool)> {
		let url = format!(
			"https://onlyfans.com/api2/v2/users/{user_id}/stories/highlights?limit=5&offset={offset}&sort=recent%3Adesc"
		);
		let response = self.get(url).send().await?;
		let body = response.text().await.map_err(reqwest_middleware::Error::Reqwest)?;

		match serde_json::from_str::<HighlightsResponse>(&body) {
			Ok(wrapped) => Ok((wrapped.list, wrapped.has_more)),
			Err(e) => {
				let snippet: String = body.chars().take(2000).collect();
				error!("Failed to decode highlights response ({}): {}", e, snippet);
				Err(reqwest_middleware::Error::Middleware(
					anyhow::anyhow!("Failed to decode highlights response: {e}")
				))
			}
		}
	}

	/// Full contents of one highlight reel, with every contained story's
	/// media flattened into a single downloadable bundle.
	pub async fn get_highlight(&self, highlight_id: u64) -> reqwest_middleware::Result<Highlight> {
		let url = format!("https://onlyfans.com/api2/v2/stories/highlights/{highlight_id}");
		let response = self.get(url).send().await?;
		let body = response.text().await.map_err(reqwest_middleware::Error::Reqwest)?;

		match serde_json::from_str::<HighlightDetail>(&body) {
			Ok(detail) => {
				let media = detail.stories.into_iter().flat_map(|s| s.media).collect();
				Ok(Highlight { id: detail.id, title: detail.title, created_at: detail.created_at, media })
			}
			Err(e) => {
				let snippet: String = body.chars().take(2000).collect();
				error!("Failed to decode highlight detail response ({}): {}", e, snippet);
				Err(reqwest_middleware::Error::Middleware(
					anyhow::anyhow!("Failed to decode highlight detail response: {e}")
				))
			}
		}
	}

	/// A creator's custom content labels/folders, plus the built-in
	/// "Archive" pseudo-label (`id: "archived"`).
	pub async fn get_labels<I: fmt::Display>(&self, user_id: I, offset: u64) -> reqwest_middleware::Result<(Vec<Label>, bool)> {
		let url = format!("https://onlyfans.com/api2/v2/users/{user_id}/labels?limit=10&offset={offset}&non-empty=1");
		let response = self.get(url).send().await?;
		let body = response.text().await.map_err(reqwest_middleware::Error::Reqwest)?;

		match serde_json::from_str::<LabelsResponse>(&body) {
			Ok(wrapped) => Ok((wrapped.list, wrapped.has_more)),
			Err(e) => {
				let snippet: String = body.chars().take(2000).collect();
				error!("Failed to decode labels response ({}): {}", e, snippet);
				Err(reqwest_middleware::Error::Middleware(
					anyhow::anyhow!("Failed to decode labels response: {e}")
				))
			}
		}
	}

	/// Posts filed under one specific label/folder.
	pub async fn get_posts_by_label<I: fmt::Display>(&self, user_id: I, label_id: &LabelId, before_publish_time: Option<DateTime<Utc>>) -> reqwest_middleware::Result<Vec<Post>> {
		let mut url = format!(
			"https://onlyfans.com/api2/v2/users/{user_id}/posts?limit=10&order=publish_date_desc&skip_users=all&format=infinite&label={label_id}&counters=0"
		);
		if let Some(time) = before_publish_time {
			url.push_str(&format!("&beforePublishTime={}.000000", time.timestamp()));
		}

		let response = self.get(url).send().await?;
		let body = response.text().await.map_err(reqwest_middleware::Error::Reqwest)?;

		match serde_json::from_str::<LabelPostsResponse>(&body) {
			Ok(wrapped) => Ok(wrapped.list),
			Err(_) => {
				// Maybe it's actually a bare array like the unlabeled
				// posts endpoint, not the {list, hasMore} envelope.
				match serde_json::from_str::<Vec<Post>>(&body) {
					Ok(list) => Ok(list),
					Err(e) => {
						let snippet: String = body.chars().take(2000).collect();
						error!("Failed to decode label posts response ({}): {}", e, snippet);
						Err(reqwest_middleware::Error::Middleware(
							anyhow::anyhow!("Failed to decode label posts response: {e}")
						))
					}
				}
			}
		}
	}
}
