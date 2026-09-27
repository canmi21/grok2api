//! The OpenAI-shaped HTTP surface; spec/api.md.

use std::convert::Infallible;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::extract::{DefaultBodyLimit, Request, State};
use axum::http::{StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;

use crate::message;
use crate::turn::{self, Bridge, Update};

/// Images arrive inline as data URLs, so a request can be far larger than axum's 2 MB default.
const BODY_LIMIT: usize = 32 * 1024 * 1024;

#[derive(Clone)]
struct AppState {
	bridge: Arc<Bridge>,
	api_key: Arc<str>,
}

pub fn router(bridge: Arc<Bridge>, api_key: String) -> Router {
	let state = AppState { bridge, api_key: api_key.into() };
	Router::new()
		.route("/v1/models", get(models))
		.route("/v1/chat/completions", post(chat_completions))
		.layer(middleware::from_fn_with_state(state.clone(), authorize))
		.layer(DefaultBodyLimit::max(BODY_LIMIT))
		.with_state(state)
}

/// An error in OpenAI's shape.
struct ApiError {
	status: StatusCode,
	kind: &'static str,
	code: Option<&'static str>,
	message: String,
}

impl ApiError {
	fn invalid(message: impl Into<String>) -> Self {
		Self {
			status: StatusCode::BAD_REQUEST,
			kind: "invalid_request_error",
			code: None,
			message: message.into(),
		}
	}
}

impl IntoResponse for ApiError {
	fn into_response(self) -> Response {
		let body = json!({ "error": { "message": self.message, "type": self.kind, "param": null, "code": self.code } });
		(self.status, Json(body)).into_response()
	}
}

async fn authorize(State(state): State<AppState>, request: Request, next: Next) -> Response {
	let presented = request
		.headers()
		.get(header::AUTHORIZATION)
		.and_then(|value| value.to_str().ok())
		.and_then(|value| value.strip_prefix("Bearer "));
	if presented.is_some_and(|key| constant_time_eq(key.as_bytes(), state.api_key.as_bytes())) {
		return next.run(request).await;
	}
	ApiError {
		status: StatusCode::UNAUTHORIZED,
		kind: "invalid_request_error",
		code: Some("invalid_api_key"),
		message: "a valid API key is required".into(),
	}
	.into_response()
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
	a.len() == b.len() && a.iter().zip(b).fold(0u8, |diff, (x, y)| diff | (x ^ y)) == 0
}

async fn models(State(state): State<AppState>) -> Json<Value> {
	let data: Vec<Value> = state
		.bridge
		.agent()
		.info
		.models
		.iter()
		.map(|id| json!({ "id": id, "object": "model", "created": 0, "owned_by": "xai" }))
		.collect();
	Json(json!({ "object": "list", "data": data }))
}

async fn chat_completions(
	State(state): State<AppState>,
	Json(body): Json<Value>,
) -> Result<Response, ApiError> {
	// Everything else a client may send is ignored; `n` alone changes the response's shape.
	if body["n"].as_u64().is_some_and(|n| n > 1) {
		return Err(ApiError::invalid("n greater than 1 is not supported"));
	}
	let messages =
		body["messages"].as_array().ok_or_else(|| ApiError::invalid("messages is required"))?;
	let conversation = message::parse(messages).map_err(ApiError::invalid)?;
	let agent = state.bridge.agent();
	let info = &agent.info;
	let model = body["model"]
		.as_str()
		.filter(|model| !model.is_empty())
		.unwrap_or(&info.default_model)
		.to_owned();
	if !info.models.contains(&model) {
		return Err(ApiError {
			status: StatusCode::NOT_FOUND,
			kind: "invalid_request_error",
			code: Some("model_not_found"),
			message: format!("the model {model} does not exist; see /v1/models"),
		});
	}
	let effort = body["reasoning_effort"].as_str().map(str::to_owned);
	let stream = body["stream"].as_bool().unwrap_or(false);
	let include_usage = body["stream_options"]["include_usage"].as_bool().unwrap_or(false);

	let updates = state
		.bridge
		.start(turn::Request { conversation, model: model.clone(), effort })
		.await
		.map_err(|error| {
			tracing::error!(%error, "a completion could not start");
			ApiError {
				status: StatusCode::BAD_GATEWAY,
				kind: "api_error",
				code: None,
				message: error.to_string(),
			}
		})?;
	let head =
		Head { id: format!("chatcmpl-{}", uuid::Uuid::new_v4().simple()), created: now(), model };
	Ok(if stream {
		streamed(head, updates, include_usage).into_response()
	} else {
		whole(head, updates).await?.into_response()
	})
}

struct Head {
	id: String,
	created: u64,
	model: String,
}

async fn whole(head: Head, mut updates: mpsc::Receiver<Update>) -> Result<Json<Value>, ApiError> {
	let (mut content, mut reasoning) = (String::new(), String::new());
	while let Some(update) = updates.recv().await {
		match update {
			Update::Content(text) => content.push_str(&text),
			Update::Reasoning(text) => reasoning.push_str(&text),
			Update::Done { finish_reason, usage } => {
				let mut message = json!({ "role": "assistant", "content": content });
				if !reasoning.is_empty() {
					message["reasoning_content"] = reasoning.into();
				}
				return Ok(Json(json!({
					"id": head.id,
					"object": "chat.completion",
					"created": head.created,
					"model": head.model,
					"choices": [{ "index": 0, "message": message, "finish_reason": finish_reason }],
					"usage": usage,
				})));
			}
			Update::Failed(message) => {
				return Err(ApiError {
					status: StatusCode::BAD_GATEWAY,
					kind: "api_error",
					code: None,
					message,
				});
			}
		}
	}
	Err(ApiError {
		status: StatusCode::BAD_GATEWAY,
		kind: "api_error",
		code: None,
		message: "the turn ended without an answer".into(),
	})
}

/// Server-sent events in OpenAI's chunk shape, ending with `[DONE]`. When the client goes away the
/// stream is dropped, the sends into it fail, and the turn is cancelled from there.
fn streamed(
	head: Head,
	mut updates: mpsc::Receiver<Update>,
	include_usage: bool,
) -> impl IntoResponse {
	let (tx, rx) = mpsc::channel::<Result<Event, Infallible>>(64);
	tokio::spawn(async move {
		let chunk = |delta: Value, finish_reason: Value| {
			json!({
				"id": head.id,
				"object": "chat.completion.chunk",
				"created": head.created,
				"model": head.model,
				"choices": [{ "index": 0, "delta": delta, "finish_reason": finish_reason }],
			})
		};
		let send = |value: Value| {
			let tx = tx.clone();
			async move { tx.send(Ok(Event::default().data(value.to_string()))).await.is_ok() }
		};
		if !send(chunk(json!({ "role": "assistant", "content": "" }), Value::Null)).await {
			return;
		}
		while let Some(update) = updates.recv().await {
			let delivered = match update {
				Update::Content(text) => send(chunk(json!({ "content": text }), Value::Null)).await,
				Update::Reasoning(text) => {
					send(chunk(json!({ "reasoning_content": text }), Value::Null)).await
				}
				Update::Done { finish_reason, usage } => {
					send(chunk(json!({}), finish_reason.into())).await;
					if include_usage {
						let mut last = chunk(json!({}), Value::Null);
						last["choices"] = json!([]);
						last["usage"] = usage;
						send(last).await;
					}
					break;
				}
				Update::Failed(message) => {
					send(
						json!({ "error": { "message": message, "type": "api_error", "param": null, "code": null } }),
					)
					.await;
					break;
				}
			};
			if !delivered {
				return;
			}
		}
		let _ = tx.send(Ok(Event::default().data("[DONE]"))).await;
	});
	Sse::new(ReceiverStream::new(rx)).keep_alive(KeepAlive::default())
}

fn now() -> u64 {
	SystemTime::now().duration_since(UNIX_EPOCH).map(|elapsed| elapsed.as_secs()).unwrap_or(0)
}
