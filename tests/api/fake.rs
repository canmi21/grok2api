//! A stand-in for `grok agent stdio`: enough of ACP for grok2api to run against, with every answer
//! saying what the agent saw, so the tests can check what grok2api sent without spending anything.
//!
//! An answer reads `session=<id> turn=<n> model=<m> effort=<e> images=<n> schema=<bool> said=<text>`.
//! A prompt containing `SLOW` streams for seconds and honors cancellation, one containing `TOOL`
//! asks permission for a tool first, and `STATS` answers with what the agent has counted.

use std::collections::{HashMap, HashSet};
use std::io::{BufRead, Write};
use std::sync::mpsc::{Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};

const MODELS: [&str; 2] = ["fake-1", "fake-2"];
const EFFORTS: [&str; 3] = ["low", "medium", "high"];

#[derive(Default)]
struct Session {
	turn: u64,
	model: String,
	effort: String,
}

#[derive(Default)]
struct State {
	sessions: HashMap<String, Session>,
	cancelled: HashSet<String>,
	cancels: u64,
	closes: u64,
	/// Replies the client owes to requests this agent sent, by id.
	waiting: HashMap<u64, Sender<Value>>,
}

type Shared = Arc<Mutex<State>>;
type Out = Arc<Mutex<std::io::Stdout>>;

fn send(out: &Out, message: Value) {
	let mut out = out.lock().unwrap();
	writeln!(out, "{message}").unwrap();
	out.flush().unwrap();
}

fn update(out: &Out, session: &str, update: Value) {
	send(
		out,
		json!({ "jsonrpc": "2.0", "method": "session/update", "params": { "sessionId": session, "update": update } }),
	);
}

fn chunk(kind: &str, text: &str) -> Value {
	json!({ "sessionUpdate": kind, "content": { "type": "text", "text": text } })
}

pub fn run() {
	let state: Shared = Arc::default();
	let out: Out = Arc::new(Mutex::new(std::io::stdout()));
	let mut next_session = 0;
	for line in std::io::stdin().lock().lines() {
		let Ok(line) = line else { break };
		let message: Value = serde_json::from_str(&line).unwrap();
		let id = message["id"].clone();
		let params = &message["params"];
		let reply = |result: Value| json!({ "jsonrpc": "2.0", "id": id, "result": result });
		let refuse = |text: &str| json!({ "jsonrpc": "2.0", "id": id, "error": { "code": -32602, "message": "Invalid params", "data": text } });
		match message["method"].as_str() {
			None => {
				let waiting = state.lock().unwrap().waiting.remove(&id.as_u64().unwrap());
				if let Some(tx) = waiting {
					let _ = tx.send(message["result"].clone());
				}
			}
			Some("initialize") => send(
				&out,
				reply(json!({ "_meta": {
					"agentVersion": "0.0.1",
					"defaultAuthMethodId": "cached_token",
					"modelState": { "currentModelId": "fake-1", "availableModels": [
						{ "modelId": "fake-1", "name": "Fake One" },
						{ "modelId": "fake-2", "name": "Fake Two" },
					] },
				} })),
			),
			Some("session/new") => {
				next_session += 1;
				let session = format!("fake-{next_session}");
				let profiled =
					params["_meta"]["agentProfile"].as_str().is_some_and(|name| name.starts_with("grok2api"));
				let tools = if profiled { 698 } else { 8831 };
				eprintln!(
					"INFO session.context_snapshot: session_context_snapshot: emitted model=\"fake-1\" skills_tokens=0 system_prompt_tokens=2 tool_definitions_tokens={tools} mcp_tokens=0 agents_md_tokens=0 workflows_tokens=0 skills_count=0"
				);
				let fresh = Session { model: "fake-1".into(), effort: "high".into(), ..Default::default() };
				state.lock().unwrap().sessions.insert(session.clone(), fresh);
				send(&out, reply(json!({ "sessionId": session })));
			}
			Some("session/set_config_option") => {
				let (config, value) =
					(params["configId"].as_str().unwrap(), params["value"].as_str().unwrap());
				let valid = match config {
					"model" => MODELS.contains(&value),
					"reasoning_effort" => EFFORTS.contains(&value),
					_ => false,
				};
				if !valid {
					send(&out, refuse("unknown value"));
					continue;
				}
				let mut state = state.lock().unwrap();
				let session = state.sessions.get_mut(params["sessionId"].as_str().unwrap()).unwrap();
				match config {
					"model" => session.model = value.into(),
					_ => session.effort = value.into(),
				}
				send(&out, reply(json!({})));
			}
			Some("session/prompt") => {
				let (state, out, params) = (state.clone(), out.clone(), params.clone());
				std::thread::spawn(move || prompt(&state, &out, id, &params));
			}
			Some("session/cancel") => {
				let mut state = state.lock().unwrap();
				state.cancels += 1;
				state.cancelled.insert(params["sessionId"].as_str().unwrap().into());
			}
			Some("session/close") => {
				state.lock().unwrap().closes += 1;
				send(&out, reply(json!({})));
			}
			Some(_) => send(
				&out,
				json!({ "jsonrpc": "2.0", "id": id, "error": { "code": -32601, "message": "unknown" } }),
			),
		}
	}
}

fn prompt(state: &Shared, out: &Out, id: Value, params: &Value) {
	let session = params["sessionId"].as_str().unwrap().to_owned();
	let blocks = params["prompt"].as_array().unwrap();
	let said: Vec<&str> = blocks.iter().filter_map(|block| block["text"].as_str()).collect();
	let said = said.join(" ");
	let images = blocks.iter().filter(|block| block["type"] == "image").count();
	let schema = params["_meta"].get("outputSchema").is_some();
	let done = |stop: &str| {
		let meta = json!({ "inputTokens": 100, "outputTokens": 10, "cachedReadTokens": 40, "reasoningTokens": 4 });
		send(
			out,
			json!({ "jsonrpc": "2.0", "id": id, "result": { "stopReason": stop, "_meta": meta } }),
		);
	};

	if said.contains("STATS") {
		let text = {
			let state = state.lock().unwrap();
			format!("cancels={} closes={}", state.cancels, state.closes)
		};
		update(out, &session, chunk("agent_message_chunk", &text));
		return done("end_turn");
	}
	if said.contains("SLOW") {
		for _ in 0..100 {
			if state.lock().unwrap().cancelled.remove(&session) {
				return done("cancelled");
			}
			update(out, &session, chunk("agent_message_chunk", "."));
			std::thread::sleep(Duration::from_millis(50));
		}
		return done("end_turn");
	}
	let mut permission = String::new();
	if said.contains("TOOL") {
		let (tx, rx) = channel();
		state.lock().unwrap().waiting.insert(900, tx);
		send(
			out,
			json!({ "jsonrpc": "2.0", "id": 900, "method": "session/request_permission", "params": {
			"sessionId": session,
			"toolCall": { "title": "rm -rf /" },
			"options": [{ "optionId": "allow", "kind": "allow_once" }, { "optionId": "deny", "kind": "reject_once" }],
		} }),
		);
		let outcome = rx.recv_timeout(Duration::from_secs(5)).unwrap_or_default();
		permission =
			format!(" permission={}", outcome["outcome"]["optionId"].as_str().unwrap_or("none"));
	}

	let text = {
		let mut state = state.lock().unwrap();
		let entry = state.sessions.get_mut(&session).unwrap();
		entry.turn += 1;
		format!(
			"session={session} turn={} model={} effort={} images={images} schema={schema}{permission} said={said}",
			entry.turn, entry.model, entry.effort
		)
	};
	update(out, &session, chunk("agent_thought_chunk", "thinking"));
	let middle = (0..=text.len() / 2).rev().find(|&index| text.is_char_boundary(index)).unwrap();
	let (head, tail) = text.split_at(middle);
	update(out, &session, chunk("agent_message_chunk", head));
	update(out, &session, chunk("agent_message_chunk", tail));
	done("end_turn");
}
