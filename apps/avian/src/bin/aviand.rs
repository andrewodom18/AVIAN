use std::{path::PathBuf, sync::Arc};

use anyhow::Result;
use avian::{api, configuration::Configuration, state::ApplicationState};
use clap::Parser;

#[derive(Parser)]
#[command(version = avian::version::BUILD_VERSION)]
#[command(about = "AVIAN local connectivity API and owned runtime control.")]
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
    let mode = if let Some(runtime) = &state.runtime {
        match runtime.configuration().execution {
            avian_runtime::ExecutionMode::IsolatedNamespace => "isolated_runtime",
            avian_runtime::ExecutionMode::Native => "native_runtime",
        }
    } else {
        "observation_only"
    };
    tracing::info!(address = %listener.local_addr()?, mode, "Local API listening");
    let exchange = configuration.service_exchange.clone().map(|configuration| {
        tokio::spawn(avian::service_exchange::run(state.clone(), configuration))
    });
    let connectivity = tokio::spawn(state.clone().watch_connectivity());
    let watcher = tokio::spawn(Arc::clone(&state).watch_membership(configuration));
    let outcome = axum::serve(listener, api::router(state.clone()))
        .with_graceful_shutdown(shutdown_signal())
        .await;
    watcher.abort();
    connectivity.abort();
    if let Some(exchange) = exchange {
        exchange.abort();
    }
    if let Some(runtime) = &state.runtime {
        runtime.stop().await?;
    }
    outcome?;
    Ok(())
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut terminate) => {
                tokio::select! { _ = tokio::signal::ctrl_c() => {}, _ = terminate.recv() => {} }
            }
            Err(_) => {
                let _ = tokio::signal::ctrl_c().await;
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}
