use std::{path::PathBuf, sync::Arc};

use anyhow::Result;
use avian::{api, configuration::Configuration, state::ApplicationState};
use clap::Parser;

#[derive(Parser)]
#[command(about = "AVIAN local observation API. Does not change network ownership.")]
struct Arguments {
    #[arg(long)]
    config: PathBuf,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "avian=info".into()),
        )
        .init();
    let arguments = Arguments::parse();
    let configuration = Configuration::load(&arguments.config)?;
    let state = ApplicationState::load(&configuration)?;
    let listener = tokio::net::TcpListener::bind(configuration.listen).await?;
    tracing::info!(address = %listener.local_addr()?, mode = "observation_only", "Local API listening");
    let watcher = tokio::spawn(Arc::clone(&state).watch_membership(configuration));
    let outcome = axum::serve(listener, api::router(state))
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await;
    watcher.abort();
    outcome?;
    Ok(())
}
