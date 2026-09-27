//! The clean environment the agent runs in. Most of what the CLI puts around a prompt is a coding
//! agent's context, and this is what takes it away; spec/bridge.md, "The agent is a coding agent".

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use tokio::process::Command;

/// The agent definition every session is created with; see `PROFILE`.
pub const PROFILE_NAME: &str = "grok2api";

/// Names one tool rather than none: an empty or unknown `tools` list gives the full tool set,
/// and so does a definition that fails to parse. The startup check is what proves this applied.
const PROFILE: &str = "---
name: grok2api
description: Plain chat for grok2api. No tools.
tools: search_tool
agents_md: false
---

You are a helpful assistant.
";

/// Read by grok's log filter: warnings, plus the per-session context breakdown the startup
/// check reads. Plain `info` logs every streamed token.
const AGENT_LOG_FILTER: &str = "warn,[session.context_snapshot]=info";

/// Passed through from this process to the agent; everything else is withheld, so nothing the
/// host sets -- an API key, a GROK_* switch -- changes what the agent does.
const PASSED_THROUGH: &[&str] = &[
	"PATH",
	"LANG",
	"TZ",
	"SSL_CERT_FILE",
	"SSL_CERT_DIR",
	"HTTP_PROXY",
	"HTTPS_PROXY",
	"NO_PROXY",
	"http_proxy",
	"https_proxy",
	"no_proxy",
];

pub struct Environment {
	/// The agent's HOME, empty on purpose: discovery of ~/.claude, ~/.cursor and their plugins
	/// keys off it, and nothing else turns plugin discovery off.
	pub home: PathBuf,
	/// The CLI's own state, credentials included.
	pub grok_home: PathBuf,
	/// The working directory sessions are created in, also empty.
	pub workspace: PathBuf,
}

impl Environment {
	pub fn prepare(data_dir: &Path) -> Result<Self> {
		let root = absolute(data_dir)?;
		let environment = Self {
			home: root.join("home"),
			grok_home: root.join("grok"),
			workspace: root.join("workspace"),
		};
		for dir in [&environment.home, &environment.grok_home, &environment.workspace] {
			std::fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
		}
		let agents = environment.grok_home.join("agents");
		std::fs::create_dir_all(&agents)?;
		std::fs::write(agents.join(format!("{PROFILE_NAME}.md")), PROFILE)?;
		Ok(environment)
	}

	/// `grok agent stdio`, with the environment cleared and set to the one above.
	pub fn agent_command(&self, grok_bin: &Path) -> Command {
		let mut command = Command::new(grok_bin);
		command.args(["agent", "stdio"]).current_dir(&self.workspace).env_clear();
		for name in PASSED_THROUGH {
			if let Ok(value) = std::env::var(name) {
				command.env(name, value);
			}
		}
		command
			.env("HOME", &self.home)
			.env("GROK_HOME", &self.grok_home)
			.env("GROK_WORKFLOWS", "0")
			.env("GROK_SUBAGENTS", "0")
			.env("GROK_MEMORY", "0")
			// grok2api runs the update cycle itself; spec/deployment.md.
			.env("GROK_DISABLE_AUTOUPDATER", "1")
			.env("RUST_LOG", AGENT_LOG_FILTER);
		command
	}

	/// What a person runs to sign this environment in; printed when the check finds it signed out.
	pub fn login_hint(&self, grok_bin: &Path) -> String {
		format!("GROK_HOME={} {} login --device-auth", self.grok_home.display(), grok_bin.display())
	}
}

fn absolute(path: &Path) -> Result<PathBuf> {
	std::fs::create_dir_all(path).with_context(|| format!("cannot create {}", path.display()))?;
	path.canonicalize().with_context(|| format!("cannot resolve {}", path.display()))
}
