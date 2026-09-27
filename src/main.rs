mod agent;
mod check;
mod config;
mod environment;
mod http;
mod message;
mod sessions;
mod transcript;
mod turn;

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use tracing_subscriber::EnvFilter;

use crate::agent::Agent;
use crate::config::Config;
use crate::environment::Environment;
use crate::sessions::Pool;
use crate::turn::Bridge;

/// How often idle sessions are looked at for expiry.
const EXPIRY_SWEEP: Duration = Duration::from_secs(60);

#[tokio::main]
async fn main() -> Result<()> {
	tracing_subscriber::fmt()
		.with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
		.init();

	let config = Config::from_env()?;
	let environment = Environment::prepare(&config.data_dir)?;
	let (agent, mut log) = Agent::spawn(&environment, &config.grok_bin)?;
	let info = agent.initialize().await.context("the agent did not initialize")?;
	tracing::info!(version = %info.version, models = ?info.models, "agent started");
	if !info.signed_in {
		bail!("the agent is not signed in; run `{}` once", environment.login_hint(&config.grok_bin));
	}
	let snapshot =
		check::run(&agent, &environment, &mut log).await.context("the startup check failed")?;
	tracing::info!(?snapshot, "clean environment verified");
	// The check needed the log; nothing reads it from here on, and a full channel would grow.
	tokio::spawn(async move { while log.recv().await.is_some() {} });

	let bridge = Arc::new(Bridge { agent, info, pool: Pool::new(config.session_idle), environment });
	let sweeper = bridge.clone();
	tokio::spawn(async move {
		let mut interval = tokio::time::interval(EXPIRY_SWEEP);
		loop {
			interval.tick().await;
			sweeper.expire().await;
		}
	});

	let listener = tokio::net::TcpListener::bind(("0.0.0.0", config.port))
		.await
		.with_context(|| format!("cannot listen on port {}", config.port))?;
	tracing::info!(port = config.port, "listening on every interface");
	axum::serve(listener, http::router(bridge, config.api_key)).await?;
	Ok(())
}
