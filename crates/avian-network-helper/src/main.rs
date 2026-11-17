#[cfg(target_os = "linux")]
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };
    let arguments = std::env::args().skip(1).collect::<Vec<_>>();
    let valid = (arguments.len() == 2 || arguments.len() == 3)
        && arguments[0] == "--config"
        && (arguments.len() == 2 || arguments[2] == "--bind-socket");
    if !valid {
        return Err(
            "usage: avian-network-helper --config <root-owned.json> [--bind-socket]".into(),
        );
    }
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let mut helper =
        avian_network_helper::server::NetworkHelper::load(std::path::Path::new(&arguments[1]))?;
    let stop = Arc::new(AtomicBool::new(false));
    let stopping = Arc::clone(&stop);
    let mut task =
        tokio::task::spawn_blocking(move || helper.serve(&stopping, arguments.len() != 3));
    tokio::select! { result = &mut task => { result??; return Ok(()); }, _ = tokio::signal::ctrl_c() => (), _ = terminate.recv() => () }
    stop.store(true, Ordering::SeqCst);
    task.await??;
    // The server notices the stop flag within its bounded accept/read timeout and
    // removes only resources matching its durable ownership record.
    Ok(())
}
#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("network helper is supported only on Linux");
    std::process::exit(1);
}
