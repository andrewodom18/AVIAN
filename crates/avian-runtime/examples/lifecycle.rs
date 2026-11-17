//! Exercise process lifecycle inside an already provisioned isolated namespace.
use avian_runtime::{RuntimeConfiguration, RuntimeSupervisor};

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let configuration_path = std::env::args_os()
        .nth(1)
        .ok_or("expected a runtime JSON configuration path")?;
    let configuration: RuntimeConfiguration =
        serde_json::from_slice(&std::fs::read(configuration_path)?)?;
    let supervisor = RuntimeSupervisor::new(configuration);
    let started = supervisor.start().await?;
    println!("{}", serde_json::to_string(&started)?);
    let live = supervisor.status().await;
    if !live.engine_ready {
        return Err("engine lost readiness".into());
    }
    let peers = supervisor.inspect_peers().await?;
    let routes = supervisor.inspect_routes().await?;
    let stopped = supervisor.stop().await?;
    if stopped.process_id.is_some() || stopped.engine_ready {
        return Err("engine did not stop".into());
    }
    println!(
        "{}",
        serde_json::to_string(
            &serde_json::json!({"stopped": stopped, "peers": peers, "engine_generic_routes": routes})
        )?
    );
    Ok(())
}
