//! Validate the fixed privileged scope before requesting any network changes.
use super::*;

/// A prepared lease remains armed until the lifecycle records ownership. If a
/// cancelled request drops its result, cleanup still runs outside that request.
pub(super) struct PreparedNetwork {
    socket: Option<PathBuf>,
    operation: Option<tokio::sync::OwnedSemaphorePermit>,
    error: Option<&'static str>,
}
impl PreparedNetwork {
    pub(super) fn commit(mut self) -> (PathBuf, Option<&'static str>) {
        (
            self.socket.take().expect("prepared lease has a socket"),
            self.error,
        )
    }
}
impl Drop for PreparedNetwork {
    fn drop(&mut self) {
        if let Some(socket) = self.socket.take() {
            let operation = self.operation.take();
            std::thread::spawn(move || {
                let _ = release_blocking(socket);
                drop(operation);
            });
        }
    }
}

pub(super) fn validate_configuration(configuration: &RuntimeConfiguration) -> Result<()> {
    let prefix = configuration.overlay_prefix_length;
    if !(8..=30).contains(&prefix) {
        return Err(rejected("invalid overlay prefix"));
    }
    let mask = u32::MAX << (32 - prefix);
    let address = u32::from(configuration.overlay_address);
    let first = Ipv4Addr::from(address & mask);
    let last = Ipv4Addr::from((address & mask) | !mask);
    if !first.is_private()
        || !last.is_private()
        || address == u32::from(first)
        || address == u32::from(last)
    {
        return Err(rejected(
            "entire overlay and host address must stay inside RFC1918",
        ));
    }
    match (
        configuration.execution,
        &configuration.network_helper_socket,
    ) {
        (ExecutionMode::Native, Some(socket))
            if socket.is_absolute() && cfg!(target_os = "linux") =>
        {
            if configuration.listen.port() < 1024 {
                return Err(rejected("managed listener must use an unprivileged port"));
            }
        }
        (ExecutionMode::Native, _) => {
            return Err(rejected(
                "native Linux execution requires a network helper socket",
            ));
        }
        (ExecutionMode::IsolatedNamespace, Some(_)) => {
            return Err(rejected(
                "namespace execution cannot request a native helper",
            ));
        }
        _ => {}
    }
    Ok(())
}

pub(super) async fn prepare_network(
    configuration: &RuntimeConfiguration,
    operation_slot: Arc<tokio::sync::Semaphore>,
) -> Result<PreparedNetwork> {
    let operation = operation_slot
        .acquire_owned()
        .await
        .map_err(|_| rejected("network helper operation unavailable"))?;
    let configuration = configuration.clone();
    tokio::task::spawn_blocking(move || prepare_blocking(&configuration, operation))
        .await
        .map_err(|_| rejected("network helper task failed"))?
}

#[cfg(target_os = "linux")]
fn prepare_blocking(
    configuration: &RuntimeConfiguration,
    operation: tokio::sync::OwnedSemaphorePermit,
) -> Result<PreparedNetwork> {
    if rustix::process::getuid().is_root() || rustix::process::geteuid().is_root() {
        return Err(rejected(
            "managed runtime must run as an unprivileged account",
        ));
    }
    let socket = configuration
        .network_helper_socket
        .as_ref()
        .ok_or_else(|| rejected("network helper is not configured"))?;
    let helper = avian_network_helper::client::HelperClient::new(socket)
        .map_err(|_| rejected("network helper socket is invalid"))?;
    let matches = |status: &avian_network_helper::NetworkStatus| {
        status.node_id == configuration.node_id
            && status.tun_name == configuration.interface_name
            && status.overlay_address == configuration.overlay_address
            && status.overlay_network.prefix_len() == configuration.overlay_prefix_length
            && status
                .overlay_network
                .contains(&configuration.overlay_address)
            && status.mtu == 1320
            && status.listen_port == configuration.listen.port()
    };
    let status = helper
        .status()
        .map_err(|_| rejected("network helper is unavailable"))?;
    if !matches(&status) {
        return Err(rejected(
            "network helper scope differs from runtime configuration",
        ));
    }
    // Status has established the fixed scope before this guard can release it.
    let mut lease = PreparedNetwork {
        socket: Some(socket.clone()),
        operation: Some(operation),
        error: None,
    };
    match helper.prepare_network() {
        Ok(prepared) if matches(&prepared) && prepared.active => (),
        Ok(_) => lease.error = Some("prepared network differs from configured scope"),
        Err(_) => lease.error = Some("network helper refused preparation"),
    }
    Ok(lease)
}
#[cfg(not(target_os = "linux"))]
fn prepare_blocking(
    _: &RuntimeConfiguration,
    _: tokio::sync::OwnedSemaphorePermit,
) -> Result<PreparedNetwork> {
    Err(RuntimeError::UnsupportedPlatform)
}

pub(super) async fn release_network(socket: PathBuf) -> Result<()> {
    tokio::task::spawn_blocking(move || release_blocking(socket))
        .await
        .map_err(|_| rejected("network helper release task failed"))?
}

#[cfg(target_os = "linux")]
fn release_blocking(socket: PathBuf) -> Result<()> {
    let helper = avian_network_helper::client::HelperClient::new(socket)
        .map_err(|_| rejected("network helper socket is invalid"))?;
    let status = helper
        .release_network()
        .map_err(|_| rejected("network helper release failed; owned scope retained for retry"))?;
    if status.active {
        return Err(rejected("network helper retained an active owned scope"));
    }
    Ok(())
}
#[cfg(not(target_os = "linux"))]
fn release_blocking(_: PathBuf) -> Result<()> {
    Err(RuntimeError::UnsupportedPlatform)
}
