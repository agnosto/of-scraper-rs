#![allow(async_fn_in_trait)]

use crate::OFClient;
use std::fmt;
use serde::Deserialize;
use futures_util::TryFutureExt;

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Me {
	pub name: String,
	pub id: u64,
	pub username: String,
	pub ws_auth_token: String,
	pub ws_url: String
}

/// A user as returned by the API.
///
/// Endpoints called with `skip_users=all` (profile feed, label posts, paid
/// posts) return a stub instead of a full user — `{"id": 10600394, "_view":
/// "s"}`. Requiring `name`/`username` there makes every item in the page fail
/// to decode, so both default to empty; callers that know who they are
/// scraping pass the real username in themselves.
#[derive(Deserialize, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct User {
	pub id: u64,
	#[serde(default)]
	pub name: String,
	#[serde(default)]
	pub username: String,
	pub avatar: Option<String>,
}

#[derive(Deserialize, Debug)]
pub struct SubscriberCategories {
	pub active: u32,
	pub muted: u32,
	pub restricted: u32,
	pub expired: u32,
	pub blocked: u32,
	pub all: u32
	// pub attention: u32, // only for subscriptions
	// activeOnline: u32, // only for subscribers
}

#[derive(Deserialize, Debug)]
pub struct Subscriptions {
	pub subscriptions: SubscriberCategories,
	pub subscribers: SubscriberCategories,
	pub bookmarks: u32,
}

pub trait IDType : fmt::Display {}
impl IDType for &str {}
impl IDType for u64 {}

impl OFClient {
	pub async fn get_user<I: IDType>(&self, user_id: I) -> reqwest_middleware::Result<User> {
		self.get(format!("https://onlyfans.com/api2/v2/users/{user_id}"))
		.send()
		.and_then(|response| response.json::<User>().map_err(Into::into))
		.await
		.inspect(|user| info!("Got user: {:?}", user))
		.inspect_err(|err| error!("Error reading user {user_id}: {err:?}"))
	}

	pub async fn subscribe<I: IDType>(&self, user_id: I) -> reqwest_middleware::Result<User> {
		self.post(format!("https://onlyfans.com/api2/v2/users/{user_id}/subscribe"))
		.send()
		.and_then(|response| response.json::<User>().map_err(Into::into))
		.await
		.inspect(|user| info!("Got user: {:?}", user))
		.inspect_err(|err| error!("Error reading user {user_id}: {err:?}"))
	}

	pub async fn get_subscriptions(&self) -> reqwest_middleware::Result<Vec<User>> {
		self.get_subscriptions_filtered(false, false).await
	}

	/// Fetch subscriptions, optionally including expired and/or restricted
	/// creators. OnlyFans' `subscribes` endpoint can only return one
	/// category per request, so the requested categories are fetched
	/// separately and deduplicated by user id.
	pub async fn get_subscriptions_filtered(&self, include_expired: bool, include_restricted: bool) -> reqwest_middleware::Result<Vec<User>> {
		let mut categories = vec!["active"];
		if include_expired { categories.push("expired"); }
		if include_restricted { categories.push("restricted"); }

		let mut all_users = Vec::new();
		let mut seen_ids: std::collections::HashSet<u64> = std::collections::HashSet::new();

		for category in categories {
			let mut offset: u64 = 0;
			let limit: u64 = 100;

			loop {
				let url = format!(
					"https://onlyfans.com/api2/v2/subscriptions/subscribes?limit={limit}&offset={offset}&type={category}"
				);
				let users: Vec<User> = self.get(&url)
					.send()
					.and_then(|response| response.json::<Vec<User>>().map_err(Into::into))
					.await?;

				if users.is_empty() { break; }
				for user in users {
					if seen_ids.insert(user.id) {
						all_users.push(user);
					}
				}

				offset += limit;
			}
		}
		Ok(all_users)
	}
}
