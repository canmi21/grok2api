//! A chat completions request's messages, split into what a session is keyed on and what is sent.
//! See spec/sessions.md for why the history is the key, and spec/api.md for what is accepted.

use std::hash::{DefaultHasher, Hash, Hasher};

use base64::Engine;
use serde_json::{Value, json};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
	User,
	Assistant,
}

#[derive(Clone, Debug)]
pub enum Part {
	Text(String),
	/// Base64 image data as a data URL carried it, with its media type.
	Image {
		mime: String,
		data: String,
	},
}

#[derive(Clone, Debug)]
pub struct Message {
	pub role: Role,
	pub parts: Vec<Part>,
}

/// What a message is compared by when a request is matched to a session. Text is trimmed,
/// because clients differ on the whitespace they keep around a reply they echo back; an image is
/// its digest, so a session holding a day of history does not hold every image in it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MessageKey {
	role: Role,
	text: String,
	images: Vec<u64>,
}

impl Message {
	pub fn key(&self) -> MessageKey {
		let mut images = Vec::new();
		for part in &self.parts {
			if let Part::Image { data, .. } = part {
				let mut hasher = DefaultHasher::new();
				data.hash(&mut hasher);
				images.push(hasher.finish());
			}
		}
		MessageKey { role: self.role, text: self.text().trim().to_owned(), images }
	}

	/// The message's text parts, joined.
	pub fn text(&self) -> String {
		let texts: Vec<&str> = self
			.parts
			.iter()
			.filter_map(|part| match part {
				Part::Text(text) => Some(text.as_str()),
				Part::Image { .. } => None,
			})
			.collect();
		texts.join("\n")
	}

	pub fn assistant(text: String) -> Self {
		Self { role: Role::Assistant, parts: vec![Part::Text(text)] }
	}
}

impl Part {
	/// The part as an ACP content block.
	pub fn block(&self) -> Value {
		match self {
			Part::Text(text) => json!({ "type": "text", "text": text }),
			Part::Image { mime, data } => json!({ "type": "image", "mimeType": mime, "data": data }),
		}
	}
}

/// A request's messages: the system text, the turns before the last, and the last, which is
/// always the user's.
pub struct Conversation {
	pub system: String,
	pub history: Vec<Message>,
	pub last: Message,
}

impl Conversation {
	pub fn history_keys(&self) -> Vec<MessageKey> {
		self.history.iter().map(Message::key).collect()
	}
}

/// Reads the `messages` array. An error is the message a 400 carries.
pub fn parse(messages: &[Value]) -> Result<Conversation, String> {
	let mut system = Vec::new();
	let mut turns = Vec::new();
	for (index, message) in messages.iter().enumerate() {
		let role = message["role"].as_str().unwrap_or_default();
		let parts =
			parse_content(&message["content"]).map_err(|error| format!("messages[{index}]: {error}"))?;
		match role {
			// A system message anywhere is part of the system prompt, which a session is created
			// with; `developer` is the newer name for the same thing.
			"system" | "developer" => system.push(Message { role: Role::User, parts }.text()),
			"user" => turns.push(Message { role: Role::User, parts }),
			"assistant" => turns.push(Message { role: Role::Assistant, parts }),
			"tool" | "function" => {
				return Err(format!("messages[{index}]: function calling is not supported"));
			}
			other => return Err(format!("messages[{index}]: unknown role {other:?}")),
		}
	}
	let last = turns.pop().ok_or("messages has no user or assistant message")?;
	if last.role != Role::User {
		return Err("the last message must be the user's".into());
	}
	Ok(Conversation { system: system.join("\n\n"), history: turns, last })
}

fn parse_content(content: &Value) -> Result<Vec<Part>, String> {
	match content {
		Value::Null => Ok(Vec::new()),
		Value::String(text) => Ok(vec![Part::Text(text.clone())]),
		Value::Array(parts) => parts.iter().map(parse_part).collect(),
		_ => Err("content must be a string or an array of parts".into()),
	}
}

fn parse_part(part: &Value) -> Result<Part, String> {
	match part["type"].as_str() {
		Some("text") => Ok(Part::Text(part["text"].as_str().unwrap_or_default().to_owned())),
		Some("image_url") => {
			let url = part["image_url"]["url"].as_str().or_else(|| part["image_url"].as_str());
			parse_data_url(url.ok_or("image_url has no url")?)
		}
		Some(other) => Err(format!("content part type {other:?} is not supported")),
		None => Err("content part has no type".into()),
	}
}

/// Only a `data:` URL is accepted; grok2api fetches nothing on a caller's behalf (spec/api.md).
fn parse_data_url(url: &str) -> Result<Part, String> {
	let rest = url.strip_prefix("data:").ok_or("only data: URLs are accepted for images")?;
	let (header, data) = rest.split_once(',').ok_or("the data URL has no data")?;
	let mime = header.strip_suffix(";base64").ok_or("the data URL must be base64")?;
	if !mime.starts_with("image/") {
		return Err(format!("{mime} is not an image type"));
	}
	base64::engine::general_purpose::STANDARD
		.decode(data)
		.map_err(|_| "the data URL is not valid base64".to_owned())?;
	Ok(Part::Image { mime: mime.to_owned(), data: data.to_owned() })
}

#[cfg(test)]
mod tests {
	use serde_json::json;

	use super::*;

	#[test]
	fn splits_system_history_and_last() {
		let conversation = parse(&[
			json!({ "role": "system", "content": "Be brief." }),
			json!({ "role": "user", "content": "Hi" }),
			json!({ "role": "assistant", "content": "Hello." }),
			json!({ "role": "user", "content": [{ "type": "text", "text": "Again" }] }),
		])
		.unwrap();
		assert_eq!(conversation.system, "Be brief.");
		assert_eq!(conversation.history.len(), 2);
		assert_eq!(conversation.last.text(), "Again");
	}

	#[test]
	fn keys_ignore_surrounding_whitespace() {
		let echoed = Message::assistant("Hello.\n".into());
		assert_eq!(echoed.key(), Message::assistant("Hello.".into()).key());
	}

	#[test]
	fn refuses_a_remote_image() {
		let part = json!({ "type": "image_url", "image_url": { "url": "https://example.com/a.png" } });
		assert!(parse_part(&part).is_err());
	}

	#[test]
	fn accepts_a_data_url_image() {
		let part =
			json!({ "type": "image_url", "image_url": { "url": "data:image/png;base64,iVBORw0K" } });
		assert!(matches!(parse_part(&part), Ok(Part::Image { .. })));
	}

	#[test]
	fn the_last_message_is_the_users() {
		assert!(parse(&[json!({ "role": "assistant", "content": "Hi" })]).is_err());
	}
}
