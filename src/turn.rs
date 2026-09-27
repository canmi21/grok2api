//! One chat completion: find or make the session, send the prompt, stream what comes back.

use std::sync::{Arc, RwLock};

use anyhow::Result;
use serde_json::{Value, json};
use tokio::sync::mpsc;

use crate::agent::Agent;
use crate::environment::Environment;
use crate::message::{Conversation, Message};
use crate::sessions;
use crate::transcript::render_history;

/// Put ahead of the client's own system prompt. The model reaches for any tool it has, and the
/// one the profile keeps is never useful to a chat; spec/bridge.md.
const BASE_SYSTEM: &str = "You are a helpful assistant. You have no tools: answer directly, and never \
	call, search for or mention a tool.";

type Session = sessions::Session<Arc<Agent>>;

/// Everything a completion needs, shared by every request.
pub struct Bridge {
	/// The agent new sessions are made in. Replaced when a newer CLI passes its check; the one it
	/// replaces lives on in the sessions it holds (spec/deployment.md).
	current: RwLock<Arc<Agent>>,
	pool: sessions::Pool<Arc<Agent>>,
	pub environment: Environment,
}

pub struct Request {
	pub conversation: Conversation,
	pub model: String,
	/// Honored when the model offers it, ignored when not (spec/api.md).
	pub effort: Option<String>,
}

pub enum Update {
	Reasoning(String),
	Content(String),
	Done { finish_reason: &'static str, usage: Value },
	Failed(String),
}

impl Bridge {
	pub fn new(agent: Arc<Agent>, environment: Environment, idle_for: std::time::Duration) -> Self {
		Self { current: RwLock::new(agent), pool: sessions::Pool::new(idle_for), environment }
	}

	pub fn agent(&self) -> Arc<Agent> {
		self.current.read().unwrap().clone()
	}

	/// Makes `agent` the one new sessions go to.
	pub fn replace(&self, agent: Arc<Agent>) {
		*self.current.write().unwrap() = agent;
	}

	/// Prepares the session and starts the prompt. An error here is the request's, returned before
	/// any byte of the response; a failure after it arrives as `Update::Failed`.
	pub async fn start(self: &Arc<Self>, request: Request) -> Result<mpsc::Receiver<Update>> {
		let Request { conversation, model, effort } = request;
		let history = conversation.history_keys();
		let (mut session, mut blocks, continued) = match self.pool.claim(&conversation.system, &history)
		{
			Some(session) => (session, Vec::new(), true),
			None => {
				let agent = self.agent();
				let id = agent
					.new_session(&self.environment.workspace, &system_prompt(&conversation.system))
					.await?;
				let model = agent.info.default_model.clone();
				let mut session = Session::new(agent, id, conversation.system.clone(), model);
				session.history = history;
				let seed = render_history(&conversation.history);
				let blocks =
					if seed.is_empty() { Vec::new() } else { vec![json!({ "type": "text", "text": seed })] };
				(session, blocks, false)
			}
		};
		if let Err(error) = self.configure(&mut session, &model, effort.as_deref()).await {
			self.discard(session);
			return Err(error);
		}
		blocks.extend(conversation.last.parts.iter().map(|part| part.block()));
		tracing::info!(
			session = %session.id,
			continued,
			turns = session.history.len() / 2 + 1,
			model = %session.model,
			"completion"
		);

		let (tx, rx) = mpsc::channel(64);
		let bridge = self.clone();
		tokio::spawn(async move { bridge.answer(session, conversation.last, blocks, tx).await });
		Ok(rx)
	}

	async fn configure(
		&self,
		session: &mut Session,
		model: &str,
		effort: Option<&str>,
	) -> Result<()> {
		if session.model != model {
			session.agent.set_option(&session.id, "model", model).await?;
			session.model = model.to_owned();
		}
		if let Some(effort) = effort
			&& session.effort.as_deref() != Some(effort)
		{
			match session.agent.set_option(&session.id, "reasoning_effort", effort).await {
				Ok(()) => session.effort = Some(effort.to_owned()),
				Err(error) => {
					tracing::debug!(%error, effort, "ignored a reasoning effort the model does not offer")
				}
			}
		}
		Ok(())
	}

	async fn answer(
		self: Arc<Self>,
		mut session: Session,
		last: Message,
		blocks: Vec<Value>,
		tx: mpsc::Sender<Update>,
	) {
		let (agent, id) = (session.agent.clone(), session.id.clone());
		let mut updates = agent.subscribe(&id);
		let prompt = agent.prompt(&id, blocks);
		tokio::pin!(prompt);
		let mut content = String::new();
		let mut client_gone = false;
		let result = loop {
			tokio::select! {
				result = &mut prompt => break result,
				Some(update) = updates.recv() => {
					if !forward(&update, &mut content, &tx).await && !client_gone {
						client_gone = true;
						tracing::info!(session = %id, "the client went away; cancelling");
						agent.cancel(&id);
					}
				}
			}
		};
		// The agent writes a prompt's updates before its result, but the two race here.
		while let Ok(update) = updates.try_recv() {
			forward(&update, &mut content, &tx).await;
		}
		agent.unsubscribe(&id);

		match result {
			Ok(result) if !client_gone => {
				session.history.push(last.key());
				session.history.push(Message::assistant(content).key());
				self.pool.release(session);
				let _ = tx
					.send(Update::Done {
						finish_reason: finish_reason(result["stopReason"].as_str()),
						usage: usage(&result["_meta"]),
					})
					.await;
			}
			// A cancelled or failed turn leaves the session holding a conversation nobody has.
			Ok(result) => {
				tracing::info!(session = %id, stop = %result["stopReason"], "the turn was cancelled; the session is discarded");
				self.discard(session);
			}
			Err(error) => {
				self.discard(session);
				let _ = tx.send(Update::Failed(error.to_string())).await;
			}
		}
	}

	fn discard(&self, session: Session) {
		tokio::spawn(async move { session.agent.close(&session.id).await });
	}

	/// Closes the sessions idle past the limit; run on a timer. A replaced agent goes when the
	/// last of its sessions does.
	pub async fn expire(&self) {
		for session in self.pool.take_expired() {
			session.agent.close(&session.id).await;
		}
	}
}

fn system_prompt(client: &str) -> String {
	if client.trim().is_empty() {
		BASE_SYSTEM.to_owned()
	} else {
		format!("{BASE_SYSTEM}\n\n{client}")
	}
}

/// Passes one `session/update` on; false when the client is no longer listening.
async fn forward(update: &Value, content: &mut String, tx: &mpsc::Sender<Update>) -> bool {
	let text = || update["content"]["text"].as_str().unwrap_or_default().to_owned();
	let message = match update["sessionUpdate"].as_str() {
		Some("agent_message_chunk") => {
			let text = text();
			content.push_str(&text);
			Update::Content(text)
		}
		Some("agent_thought_chunk") => Update::Reasoning(text()),
		Some("tool_call") => {
			tracing::warn!(tool = %update["title"], "the model called a tool");
			return true;
		}
		_ => return true,
	};
	tx.send(message).await.is_ok()
}

fn finish_reason(stop_reason: Option<&str>) -> &'static str {
	match stop_reason {
		Some("max_tokens") => "length",
		Some("refusal") => "content_filter",
		_ => "stop",
	}
}

/// ACP's `inputTokens` is the whole prompt, cache hits included, which is what OpenAI's
/// `prompt_tokens` means too.
fn usage(meta: &Value) -> Value {
	let count = |name: &str| meta[name].as_u64().unwrap_or(0);
	let (input, output) = (count("inputTokens"), count("outputTokens"));
	json!({
		"prompt_tokens": input,
		"completion_tokens": output,
		"total_tokens": input + output,
		"prompt_tokens_details": { "cached_tokens": count("cachedReadTokens") },
		"completion_tokens_details": { "reasoning_tokens": count("reasoningTokens") },
	})
}
