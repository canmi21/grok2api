//! The latest answer to each request, and the one refresh of it that may run at a time. Fast
//! response answers from here and refreshes behind (spec/twitter.md, "Fast response").

use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::sync::broadcast;

use super::fetch::Reply;

struct Latest {
	reply: Arc<Reply>,
	asked: Instant,
}

#[derive(Default)]
struct Inner {
	latest: HashMap<String, Latest>,
	refreshing: HashMap<String, broadcast::Sender<Arc<Reply>>>,
}

#[derive(Default)]
pub struct Store {
	inner: Mutex<Inner>,
}

impl Store {
	/// The latest answer to `key`, if there is one.
	pub fn latest(&self, key: &str) -> Option<Arc<Reply>> {
		let mut inner = self.inner.lock().unwrap();
		let latest = inner.latest.get_mut(key)?;
		latest.asked = Instant::now();
		Some(latest.reply.clone())
	}

	/// Refreshes `key`, or joins the refresh already running for it; the receiver gets its reply.
	pub fn refresh<F>(self: &Arc<Self>, key: &str, fetch: F) -> broadcast::Receiver<Arc<Reply>>
	where
		F: Future<Output = Reply> + Send + 'static,
	{
		let mut inner = self.inner.lock().unwrap();
		if let Some(running) = inner.refreshing.get(key) {
			return running.subscribe();
		}
		let (tx, rx) = broadcast::channel(1);
		inner.refreshing.insert(key.to_owned(), tx.clone());
		drop(inner);
		let (store, key) = (self.clone(), key.to_owned());
		tokio::spawn(async move {
			// However the fetch ends, a panic included, the key is freed for the next refresh.
			let _guard = Refreshing { store: store.clone(), key: key.clone(), sender: tx.clone() };
			let reply = Arc::new(fetch.await);
			store.keep(&key, reply.clone());
			// Freed before the send, so a request that sees this reply and asks again refreshes anew.
			store.done(&key, &tx);
			let _ = tx.send(reply);
		});
		rx
	}

	fn keep(&self, key: &str, reply: Arc<Reply>) {
		let mut inner = self.inner.lock().unwrap();
		let holds_answer = inner.latest.get(key).is_some_and(|latest| latest.reply.is_answer());
		if reply.is_answer() || !holds_answer {
			inner.latest.insert(key.to_owned(), Latest { reply, asked: Instant::now() });
		}
	}

	/// Frees `key` for the next refresh, unless a later one already holds it.
	fn done(&self, key: &str, sender: &broadcast::Sender<Arc<Reply>>) {
		let mut inner = self.inner.lock().unwrap();
		if inner.refreshing.get(key).is_some_and(|running| running.same_channel(sender)) {
			inner.refreshing.remove(key);
		}
	}

	/// Forgets what nobody has asked for in `idle`.
	pub fn sweep(&self, idle: Duration) {
		self.inner.lock().unwrap().latest.retain(|_, latest| latest.asked.elapsed() < idle);
	}
}

struct Refreshing {
	store: Arc<Store>,
	key: String,
	sender: broadcast::Sender<Arc<Reply>>,
}

impl Drop for Refreshing {
	fn drop(&mut self) {
		self.store.done(&self.key, &self.sender);
	}
}

#[cfg(test)]
mod tests {
	use axum::http::StatusCode;
	use serde_json::json;

	use super::*;
	use crate::twitter::job;

	fn reply(status: StatusCode) -> Reply {
		Reply { status, body: json!({}), cache_control: job::RECENT }
	}

	#[tokio::test]
	async fn one_refresh_runs_per_key() {
		let store = Arc::new(Store::default());
		let (release, wait) = tokio::sync::oneshot::channel::<()>();
		let mut first = store.refresh("k", async move {
			let _ = wait.await;
			reply(StatusCode::OK)
		});
		let mut second = store.refresh("k", async { panic!("a second refresh ran") });
		release.send(()).unwrap();
		assert_eq!(first.recv().await.unwrap().status, StatusCode::OK);
		assert_eq!(second.recv().await.unwrap().status, StatusCode::OK);
		assert_eq!(store.latest("k").unwrap().status, StatusCode::OK);
	}

	#[tokio::test]
	async fn a_failure_does_not_replace_an_answer() {
		let store = Arc::new(Store::default());
		store.refresh("k", async { reply(StatusCode::OK) }).recv().await.unwrap();
		store.refresh("k", async { reply(StatusCode::GATEWAY_TIMEOUT) }).recv().await.unwrap();
		assert_eq!(store.latest("k").unwrap().status, StatusCode::OK);
		store.refresh("gone", async { reply(StatusCode::GATEWAY_TIMEOUT) }).recv().await.unwrap();
		assert_eq!(
			store.latest("gone").unwrap().status,
			StatusCode::GATEWAY_TIMEOUT,
			"with nothing better, it is kept"
		);
	}

	#[test]
	fn forgets_what_nobody_asks_for() {
		let store = Store::default();
		store.keep("k", Arc::new(reply(StatusCode::OK)));
		store.sweep(Duration::ZERO);
		assert!(store.latest("k").is_none());
	}
}
