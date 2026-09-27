//! The idle sessions, and which request continues which. See spec/sessions.md.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::message::MessageKey;

/// A session not answering anything right now, with the conversation it has seen.
pub struct Session {
	pub id: String,
	pub system: String,
	pub history: Vec<MessageKey>,
	pub model: String,
	pub effort: Option<String>,
	last_used: Instant,
}

impl Session {
	pub fn new(id: String, system: String, model: String, effort: Option<String>) -> Self {
		Self { id, system, history: Vec::new(), model, effort, last_used: Instant::now() }
	}
}

/// A session is either here, idle, or held by the one request it is answering. Taking it out to
/// answer is what keeps two requests from continuing one conversation at once: the second finds
/// nothing to match and starts a session of its own.
pub struct Pool {
	idle: Mutex<Vec<Session>>,
	idle_for: Duration,
}

impl Pool {
	pub fn new(idle_for: Duration) -> Self {
		Self { idle: Mutex::default(), idle_for }
	}

	/// Takes the idle session whose conversation is exactly the one given, if there is one.
	pub fn claim(&self, system: &str, history: &[MessageKey]) -> Option<Session> {
		let mut idle = self.idle.lock().unwrap();
		let index =
			idle.iter().position(|session| session.system == system && session.history == history)?;
		Some(idle.swap_remove(index))
	}

	/// Returns a session that answered, to be continued by whichever request extends it next.
	pub fn release(&self, mut session: Session) {
		session.last_used = Instant::now();
		self.idle.lock().unwrap().push(session);
	}

	/// Removes the sessions idle past the limit and returns their ids, to be closed.
	pub fn take_expired(&self) -> Vec<String> {
		let mut idle = self.idle.lock().unwrap();
		let mut expired = Vec::new();
		idle.retain(|session| {
			let keep = session.last_used.elapsed() < self.idle_for;
			if !keep {
				expired.push(session.id.clone());
			}
			keep
		});
		expired
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::message::Message;

	fn session(id: &str, history: &[&str]) -> Session {
		let mut session = Session::new(id.into(), String::new(), "grok-4.7".into(), None);
		session.history = history.iter().map(|text| Message::assistant((*text).into()).key()).collect();
		session
	}

	#[test]
	fn claims_only_an_exact_conversation() {
		let pool = Pool::new(Duration::from_secs(60));
		pool.release(session("a", &["one", "two"]));
		let shorter = [Message::assistant("one".into()).key()];
		assert!(pool.claim("", &shorter).is_none());
		let exact = [Message::assistant("one".into()).key(), Message::assistant("two".into()).key()];
		assert_eq!(pool.claim("", &exact).unwrap().id, "a");
		assert!(pool.claim("", &exact).is_none(), "a claimed session is no longer idle");
	}

	#[test]
	fn the_system_prompt_is_part_of_the_match() {
		let pool = Pool::new(Duration::from_secs(60));
		pool.release(session("a", &[]));
		assert!(pool.claim("another system prompt", &[]).is_none());
	}

	#[test]
	fn expires_what_idled_too_long() {
		let pool = Pool::new(Duration::ZERO);
		pool.release(session("a", &[]));
		assert_eq!(pool.take_expired(), ["a"]);
	}
}
