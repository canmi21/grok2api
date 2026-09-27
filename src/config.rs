//! Settings, read from the environment because the program runs in a container
//! (spec/deployment.md).

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result, bail};

pub struct Config {
	/// The port listened on, on every interface (spec/api.md).
	pub port: u16,
	/// The one key every request must carry.
	pub api_key: String,
	/// Where the agent's clean environment and the CLI's state live; a volume in the container.
	pub data_dir: PathBuf,
	/// The Grok Build CLI to run.
	pub grok_bin: PathBuf,
	/// How long a session is kept after it last answered (spec/sessions.md).
	pub session_idle: Duration,
}

impl Config {
	pub fn from_env() -> Result<Self> {
		let api_key = std::env::var("GROK2API_API_KEY").unwrap_or_default();
		if api_key.trim().is_empty() {
			bail!("GROK2API_API_KEY is not set; every request is checked against it");
		}
		Ok(Self {
			port: parse("GROK2API_PORT", 8000)?,
			api_key,
			data_dir: var("GROK2API_DATA_DIR").unwrap_or_else(|| "data".into()).into(),
			grok_bin: var("GROK2API_GROK_BIN").unwrap_or_else(|| "grok".into()).into(),
			session_idle: Duration::from_secs(parse("GROK2API_SESSION_IDLE_SECS", 24 * 60 * 60)?),
		})
	}
}

fn var(name: &str) -> Option<String> {
	std::env::var(name).ok().filter(|value| !value.trim().is_empty())
}

fn parse<T: std::str::FromStr>(name: &str, default: T) -> Result<T>
where
	T::Err: std::error::Error + Send + Sync + 'static,
{
	match var(name) {
		Some(value) => value.trim().parse().with_context(|| format!("{name} is not valid: {value}")),
		None => Ok(default),
	}
}
