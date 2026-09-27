//! How a conversation's earlier turns are put in front of a session that never saw them.
//! spec/sessions.md, "A new session is seeded from the history".

use crate::message::Message;

/// Renders the turns before the last message as one block of text, sent to a brand-new session
/// ahead of the last message itself. It runs whenever a request carries history but continues no
/// session: a conversation the client edited, one whose session expired, any conversation after
/// a restart.
///
/// The model reads the result as part of a user message, so it has to tell the model that these
/// are earlier turns -- whose they were, and in what order -- and that the message after them is
/// the one to answer. Images in earlier turns cannot be re-sent here; `Message::text` skips them.
pub fn render_history(history: &[Message]) -> String {
	// TODO: render `history` (oldest first; each has `role` and `text()`).
	let _ = history;
	String::new()
}
