//! The resident `grok agent stdio` process, spoken to over ACP: JSON-RPC 2.0, one message per
//! line. See spec/bridge.md, "One resident agent, spoken to over ACP".

use std::collections::HashMap;
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result, anyhow};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStderr, ChildStdout};
use tokio::sync::{mpsc, oneshot};

use crate::environment::Environment;

/// An error the agent answered with, kept apart from transport failures so a caller can tell
/// "the agent said no" from "the agent is gone".
#[derive(Debug)]
pub struct RpcError {
	pub code: i64,
	pub message: String,
}

impl std::fmt::Display for RpcError {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		write!(f, "agent error {}: {}", self.code, self.message)
	}
}

impl std::error::Error for RpcError {}

type Pending = Arc<Mutex<HashMap<u64, oneshot::Sender<Result<Value, RpcError>>>>>;
type Subscribers = Arc<Mutex<HashMap<String, mpsc::UnboundedSender<Value>>>>;

/// What `initialize` told us about the agent.
pub struct AgentInfo {
	pub version: String,
	pub signed_in: bool,
	pub models: Vec<String>,
	pub default_model: String,
}

pub struct Agent {
	writer: mpsc::UnboundedSender<String>,
	pending: Pending,
	subscribers: Subscribers,
	next_id: AtomicU64,
	_child: Child,
}

impl Agent {
	/// Starts the agent. The returned receiver carries its stderr lines, which is where the
	/// startup check reads the context breakdown from.
	pub fn spawn(
		environment: &Environment,
		grok_bin: &std::path::Path,
	) -> Result<(Arc<Self>, mpsc::UnboundedReceiver<String>)> {
		let mut child = environment
			.agent_command(grok_bin)
			.stdin(Stdio::piped())
			.stdout(Stdio::piped())
			.stderr(Stdio::piped())
			.kill_on_drop(true)
			.spawn()
			.with_context(|| format!("cannot start {}", grok_bin.display()))?;
		let stdin = child.stdin.take().context("agent has no stdin")?;
		let stdout = child.stdout.take().context("agent has no stdout")?;
		let stderr = child.stderr.take().context("agent has no stderr")?;

		let (writer, mut outbox) = mpsc::unbounded_channel::<String>();
		tokio::spawn(async move {
			let mut stdin = stdin;
			while let Some(line) = outbox.recv().await {
				if stdin.write_all(line.as_bytes()).await.is_err() || stdin.flush().await.is_err() {
					break;
				}
			}
		});

		let pending: Pending = Arc::default();
		let subscribers: Subscribers = Arc::default();
		tokio::spawn(read_stdout(stdout, writer.clone(), pending.clone(), subscribers.clone()));
		let (log_tx, log_rx) = mpsc::unbounded_channel();
		tokio::spawn(read_stderr(stderr, log_tx));

		let agent =
			Arc::new(Self { writer, pending, subscribers, next_id: AtomicU64::new(1), _child: child });
		Ok((agent, log_rx))
	}

	pub async fn request(&self, method: &str, params: Value) -> Result<Value> {
		let id = self.next_id.fetch_add(1, Ordering::Relaxed);
		let (tx, rx) = oneshot::channel();
		self.pending.lock().unwrap().insert(id, tx);
		self.send(json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }))?;
		match rx.await {
			Ok(Ok(result)) => Ok(result),
			Ok(Err(error)) => Err(error.into()),
			Err(_) => Err(anyhow!("the agent exited while {method} was pending")),
		}
	}

	pub fn notify(&self, method: &str, params: Value) -> Result<()> {
		self.send(json!({ "jsonrpc": "2.0", "method": method, "params": params }))
	}

	fn send(&self, message: Value) -> Result<()> {
		self.writer.send(format!("{message}\n")).map_err(|_| anyhow!("the agent's stdin is closed"))
	}

	/// Routes the session's `session/update` notifications to the returned receiver until
	/// `unsubscribe`. One subscriber per session: a session answers one prompt at a time.
	pub fn subscribe(&self, session_id: &str) -> mpsc::UnboundedReceiver<Value> {
		let (tx, rx) = mpsc::unbounded_channel();
		self.subscribers.lock().unwrap().insert(session_id.to_owned(), tx);
		rx
	}

	pub fn unsubscribe(&self, session_id: &str) {
		self.subscribers.lock().unwrap().remove(session_id);
	}

	pub async fn initialize(&self) -> Result<AgentInfo> {
		let result = self
			.request(
				"initialize",
				json!({
					"protocolVersion": 1,
					"clientCapabilities": { "fs": { "readTextFile": false, "writeTextFile": false }, "terminal": false },
				}),
			)
			.await?;
		let meta = &result["_meta"];
		let state = &meta["modelState"];
		let models: Vec<String> = state["availableModels"]
			.as_array()
			.map(|models| {
				models.iter().filter_map(|model| model["modelId"].as_str().map(str::to_owned)).collect()
			})
			.unwrap_or_default();
		Ok(AgentInfo {
			version: meta["agentVersion"].as_str().unwrap_or("unknown").to_owned(),
			signed_in: meta["defaultAuthMethodId"].as_str() == Some("cached_token"),
			default_model: state["currentModelId"]
				.as_str()
				.map(str::to_owned)
				.or_else(|| models.first().cloned())
				.unwrap_or_default(),
			models,
		})
	}

	/// A session in the clean workspace, on grok2api's profile, with `system` replacing the agent's
	/// own system prompt outright.
	pub async fn new_session(&self, cwd: &std::path::Path, system: &str) -> Result<String> {
		let result = self
			.request(
				"session/new",
				json!({
					"cwd": cwd,
					"mcpServers": [],
					"_meta": {
						"systemPromptOverride": system,
						"agentProfile": crate::environment::PROFILE_NAME,
					},
				}),
			)
			.await?;
		result["sessionId"].as_str().map(str::to_owned).context("session/new returned no sessionId")
	}

	pub async fn set_option(&self, session_id: &str, config_id: &str, value: &str) -> Result<()> {
		self
			.request(
				"session/set_config_option",
				json!({ "sessionId": session_id, "configId": config_id, "value": value }),
			)
			.await
			.map(drop)
	}

	/// Sends one prompt and waits for its end; the streamed content arrives through `subscribe`.
	pub async fn prompt(&self, session_id: &str, blocks: Vec<Value>) -> Result<Value> {
		self.request("session/prompt", json!({ "sessionId": session_id, "prompt": blocks })).await
	}

	pub fn cancel(&self, session_id: &str) {
		let _ = self.notify("session/cancel", json!({ "sessionId": session_id }));
	}

	pub async fn close(&self, session_id: &str) {
		if let Err(error) = self.request("session/close", json!({ "sessionId": session_id })).await {
			tracing::debug!(session_id, %error, "closing a session failed");
		}
	}
}

async fn read_stdout(
	stdout: ChildStdout,
	writer: mpsc::UnboundedSender<String>,
	pending: Pending,
	subscribers: Subscribers,
) {
	let mut lines = BufReader::new(stdout).lines();
	while let Ok(Some(line)) = lines.next_line().await {
		let Ok(message) = serde_json::from_str::<Value>(&line) else {
			tracing::warn!(line, "the agent wrote a line that is not JSON");
			continue;
		};
		let id = message.get("id").and_then(Value::as_u64);
		match (message.get("method").and_then(Value::as_str), id) {
			(Some(method), Some(_)) => answer_request(&writer, method, &message),
			(Some("session/update"), None) => {
				let params = &message["params"];
				let Some(session_id) = params["sessionId"].as_str() else { continue };
				if let Some(tx) = subscribers.lock().unwrap().get(session_id) {
					let _ = tx.send(params["update"].clone());
				}
			}
			(Some(_), None) => {}
			(None, Some(id)) => {
				let Some(tx) = pending.lock().unwrap().remove(&id) else { continue };
				let outcome = match message.get("error") {
					Some(error) => Err(RpcError {
						code: error["code"].as_i64().unwrap_or(0),
						message: match error.get("data") {
							Some(Value::String(data)) => {
								format!("{}: {data}", error["message"].as_str().unwrap_or(""))
							}
							_ => error["message"].as_str().unwrap_or("").to_owned(),
						},
					}),
					None => Ok(message.get("result").cloned().unwrap_or(Value::Null)),
				};
				let _ = tx.send(outcome);
			}
			(None, None) => {}
		}
	}
	// Every pending request fails as its sender drops. A server whose agent is gone cannot answer
	// anything, so it stops and leaves the restart to whatever runs it; spec/bridge.md.
	tracing::error!("the agent exited");
	std::process::exit(1);
}

/// The agent asks the client for things only when it wants a tool to run. grok2api offers no
/// capabilities and approves nothing; spec/bridge.md, "The model reaches for tools".
fn answer_request(writer: &mpsc::UnboundedSender<String>, method: &str, message: &Value) {
	let id = &message["id"];
	let reply = if method == "session/request_permission" {
		let reject = message["params"]["options"].as_array().and_then(|options| {
			options
				.iter()
				.find(|option| option["kind"].as_str().is_some_and(|kind| kind.starts_with("reject")))
		});
		tracing::warn!(tool = %message["params"]["toolCall"]["title"], "refused a tool the agent asked to run");
		let outcome = match reject.and_then(|option| option["optionId"].as_str()) {
			Some(option_id) => json!({ "outcome": "selected", "optionId": option_id }),
			None => json!({ "outcome": "cancelled" }),
		};
		json!({ "jsonrpc": "2.0", "id": id, "result": { "outcome": outcome } })
	} else {
		tracing::debug!(method, "refused an agent request");
		json!({ "jsonrpc": "2.0", "id": id, "error": { "code": -32601, "message": "not supported" } })
	};
	let _ = writer.send(format!("{reply}\n"));
}

async fn read_stderr(stderr: ChildStderr, log: mpsc::UnboundedSender<String>) {
	let mut lines = BufReader::new(stderr).lines();
	while let Ok(Some(line)) = lines.next_line().await {
		let line = strip_ansi(&line);
		if line.contains("WARN") || line.contains("ERROR") {
			tracing::warn!(target: "grok", "{line}");
		} else {
			tracing::debug!(target: "grok", "{line}");
		}
		let _ = log.send(line);
	}
}

fn strip_ansi(line: &str) -> String {
	let mut out = String::with_capacity(line.len());
	let mut chars = line.chars();
	while let Some(c) = chars.next() {
		if c == '\u{1b}' {
			for c in chars.by_ref() {
				if c.is_ascii_alphabetic() {
					break;
				}
			}
		} else {
			out.push(c);
		}
	}
	out
}

#[cfg(test)]
mod tests {
	#[test]
	fn strips_color_codes() {
		assert_eq!(super::strip_ansi("\u{1b}[32m INFO\u{1b}[0m done"), " INFO done");
	}
}
