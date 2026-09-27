//! How a conversation's earlier turns are put in front of a session that never saw them.
//! spec/sessions.md, "A new session is seeded from the history".

use crate::message::{Message, Part, Role};

/// Renders the turns before the last message as one block of text, sent to a brand-new session
/// ahead of the last message itself. It runs whenever a request carries history but continues no
/// session: a conversation the client edited, one whose session expired, any conversation after
/// a restart.
///
/// The model reads the result as part of a user message, so it says what the block is, marks each
/// turn with whose it was, and ends by pointing at the message to answer. Tags rather than
/// `User:` prefixes, because a turn's own text may contain a line that starts that way. An image
/// cannot be re-sent here, so it is named where it was.
pub fn render_history(history: &[Message]) -> String {
	if history.is_empty() {
		return String::new();
	}
	let mut out = String::from(
		"The conversation so far, oldest first. It happened before this message; do not answer it \
		 again.\n\n",
	);
	for message in history {
		let role = match message.role {
			Role::User => "user",
			Role::Assistant => "assistant",
		};
		out.push_str(&format!("<turn role=\"{role}\">\n"));
		for part in &message.parts {
			match part {
				Part::Text(text) => out.push_str(text.trim()),
				Part::Image { .. } => out.push_str("[an image]"),
			}
			out.push('\n');
		}
		out.push_str("</turn>\n");
	}
	out.push_str("\nAnswer the message that follows.");
	out
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn nothing_to_render_is_nothing() {
		assert_eq!(render_history(&[]), "");
	}

	#[test]
	fn marks_whose_turn_each_was() {
		let history = [
			Message { role: Role::User, parts: vec![Part::Text("Hi".into())] },
			Message::assistant("Hello.".into()),
		];
		let rendered = render_history(&history);
		assert!(rendered.contains("<turn role=\"user\">\nHi\n</turn>"));
		assert!(rendered.contains("<turn role=\"assistant\">\nHello.\n</turn>"));
		assert!(rendered.ends_with("Answer the message that follows."));
	}
}
