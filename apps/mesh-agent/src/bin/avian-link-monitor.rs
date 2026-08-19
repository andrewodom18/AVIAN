use std::collections::{BTreeMap, VecDeque};
use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::Context;
use clap::Parser;
use mesh_agent::config::{
    CalibratedLinkConfig, CliArgs, PeerProbeConfig, RadioConfig, ResolvedConfig, Underlay,
};
use mesh_agent::link_monitor_protocol;
use mesh_core::{
    LinkGeometry, LinkMetrics, LinkMonitorObservation, NodeId, PeerProbeObservation,
    RadioApiObservation, RelayLinkObservation, StreamCasterRfLink, TransportKind,
    LINK_MONITOR_SCHEMA_VERSION,
};
use serde::Deserialize;
use streamcaster_control::{StreamCasterAuth, StreamCasterClient, StreamCasterReadApi};
use tokio::net::UdpSocket;
use tokio::time::{self, MissedTickBehavior};

const PROBE_MAGIC: &[u8] = b"AVIAN-LINK-PROBE-v1\0";

#[derive(Debug, Parser)]
#[command(
    name = "avian-link-monitor",
    about = "Read-only AVIAN radio and underlay monitor",
    version
)]
struct Args {
    #[arg(long)]
    config: PathBuf,
    #[arg(long)]
    once: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Credentials {
    username: String,
    password: String,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let config = ResolvedConfig::load(CliArgs::parse_from([
        "mesh-agent",
        "--config",
        args.config
            .to_str()
            .context("configuration path is not UTF-8")?,
    ]))?;
    if !link_monitor_has_work(&config.radio) {
        println!("AVIAN radio monitoring is disabled; waiting for shutdown");
        tokio::signal::ctrl_c()
            .await
            .context("waiting for link monitor shutdown")?;
        return Ok(());
    }
    if !config.radio.enabled {
        println!("AVIAN radio APIs are disabled; running configured network probes only");
    }
    let responder = config
        .radio
        .probe_listen
        .map(spawn_probe_responder)
        .transpose()?;
    let mut history: BTreeMap<(String, Underlay), VecDeque<f64>> = BTreeMap::new();
    let mut interval = time::interval(Duration::from_secs(
        config.radio.observation_interval_seconds,
    ));
    interval.set_missed_tick_behavior(MissedTickBehavior::Skip);
    loop {
        interval.tick().await;
        let observation = observe(&config, &mut history).await;
        if let Err(error) =
            link_monitor_protocol::send(&config.sockets.link_observation, observation).await
        {
            eprintln!("Link observation delivery failed: {error}");
        }
        if args.once {
            break;
        }
    }
    if let Some(task) = responder {
        task.abort();
    }
    Ok(())
}

async fn observe(
    config: &ResolvedConfig,
    history: &mut BTreeMap<(String, Underlay), VecDeque<f64>>,
) -> LinkMonitorObservation {
    let observed_at_ms = unix_time_ms();
    let mut radios = Vec::new();
    if config.radio.enabled {
        for device in &config.radio.devices {
            radios.push(observe_radio(device, observed_at_ms).await);
        }
    }
    let mut probes = Vec::new();
    for probe in &config.radio.probes {
        let mut observation = probe_peer(probe, config.radio.probe_timeout_ms).await;
        let values = history
            .entry((probe.peer.clone(), probe.underlay))
            .or_default();
        values.push_back(observation.loss_ratio);
        while values.len() > 10 {
            values.pop_front();
        }
        if values.len() >= 3 {
            let minimum = values.iter().copied().fold(1.0_f64, f64::min);
            let maximum = values.iter().copied().fold(0.0_f64, f64::max);
            observation.stability = Some((1.0 - (maximum - minimum)).clamp(0.0, 1.0));
        }
        probes.push(observation);
    }
    let (relay_observations, degradation_reasons) =
        build_relay_observations(config, &radios, &probes, observed_at_ms);
    LinkMonitorObservation {
        schema_version: LINK_MONITOR_SCHEMA_VERSION,
        observed_at_ms,
        radios,
        probes,
        relay_observations,
        degradation_reasons,
    }
}

fn link_monitor_has_work(radio: &RadioConfig) -> bool {
    radio.enabled || radio.probe_listen.is_some() || !radio.probes.is_empty()
}

async fn observe_radio(
    device: &mesh_agent::config::RadioDeviceConfig,
    observed_at_ms: u64,
) -> RadioApiObservation {
    let mut observation = RadioApiObservation {
        name: device.name.clone(),
        observed_at_ms,
        api_fresh: false,
        capabilities: None,
        effective_settings: None,
        rf_links: Vec::new(),
        errors: Vec::new(),
    };
    let credentials = match read_credentials(&device.credentials_file) {
        Ok(value) => value,
        Err(_) => {
            observation.errors.push("credentials_unavailable".into());
            return observation;
        }
    };
    let client = match StreamCasterClient::new(
        &device.base_url,
        StreamCasterAuth::Password {
            username: credentials.username,
            password: credentials.password,
        },
    ) {
        Ok(value) => value,
        Err(_) => {
            observation
                .errors
                .push("client_configuration_invalid".into());
            return observation;
        }
    };
    let (capabilities, settings, links) = tokio::join!(
        client.read_capabilities(observed_at_ms),
        client.read_effective_settings(observed_at_ms),
        client.read_rf_links(device.local_node_id, observed_at_ms),
    );
    match capabilities {
        Ok(value) => observation.capabilities = Some(value),
        Err(_) => observation.errors.push("capabilities_unavailable".into()),
    }
    match settings {
        Ok(value) => observation.effective_settings = Some(value),
        Err(_) => observation.errors.push("settings_unavailable".into()),
    }
    match links {
        Ok(value) => observation.rf_links = value,
        Err(_) => observation.errors.push("rf_links_unavailable".into()),
    }
    observation.api_fresh = observation.errors.is_empty();
    observation
}

async fn probe_peer(probe: &PeerProbeConfig, timeout_ms: u64) -> PeerProbeObservation {
    let observed_at_ms = unix_time_ms();
    let target = match probe.address.parse::<SocketAddr>() {
        Ok(value) => value,
        Err(error) => {
            return failed_probe(probe, observed_at_ms, error.to_string());
        }
    };
    let bind = match target.ip() {
        IpAddr::V4(_) => "0.0.0.0:0",
        IpAddr::V6(_) => "[::]:0",
    };
    let socket = match UdpSocket::bind(bind).await {
        Ok(value) => value,
        Err(error) => return failed_probe(probe, observed_at_ms, error.to_string()),
    };
    let mut payload = vec![0_u8; usize::from(probe.payload_bytes)];
    payload[..PROBE_MAGIC.len()].copy_from_slice(PROBE_MAGIC);
    let started = Instant::now();
    let mut received = 0_u16;
    let mut latency_total_ms = 0.0_f64;
    let mut response = vec![0_u8; payload.len()];
    for sequence in 0..probe.packets {
        payload[PROBE_MAGIC.len()..PROBE_MAGIC.len() + 2].copy_from_slice(&sequence.to_be_bytes());
        let sent_at = Instant::now();
        if socket.send_to(&payload, target).await.is_err() {
            continue;
        }
        let result = time::timeout(
            Duration::from_millis(timeout_ms),
            socket.recv_from(&mut response),
        )
        .await;
        if let Ok(Ok((length, source))) = result {
            if source == target && response[..length] == payload[..length] {
                received = received.saturating_add(1);
                latency_total_ms += sent_at.elapsed().as_secs_f64() * 1_000.0;
            }
        }
    }
    let elapsed = started.elapsed();
    let loss_ratio = 1.0 - f64::from(received) / f64::from(probe.packets);
    let goodput_bps = (received > 0 && elapsed.as_secs_f64() > 0.0).then_some(
        ((u64::from(received) * u64::from(probe.payload_bytes) * 8) as f64 / elapsed.as_secs_f64())
            as u64,
    );
    PeerProbeObservation {
        peer: probe.peer.clone(),
        underlay: transport(probe.underlay),
        observed_at_ms,
        sample_window_ms: elapsed.as_millis().max(1).try_into().unwrap_or(u64::MAX),
        sent_packets: probe.packets,
        received_packets: received,
        latency_ms: (received > 0).then_some(latency_total_ms / f64::from(received)),
        loss_ratio,
        goodput_bps,
        stability: None,
        reachable: received > 0,
        error: (received == 0).then_some("probe timeout".into()),
    }
}

fn failed_probe(
    probe: &PeerProbeConfig,
    observed_at_ms: u64,
    error: String,
) -> PeerProbeObservation {
    PeerProbeObservation {
        peer: probe.peer.clone(),
        underlay: transport(probe.underlay),
        observed_at_ms,
        sample_window_ms: 1,
        sent_packets: probe.packets,
        received_packets: 0,
        latency_ms: None,
        loss_ratio: 1.0,
        goodput_bps: None,
        stability: None,
        reachable: false,
        error: Some(error),
    }
}

fn build_relay_observations(
    config: &ResolvedConfig,
    radios: &[RadioApiObservation],
    probes: &[PeerProbeObservation],
    observed_at_ms: u64,
) -> (Vec<RelayLinkObservation>, Vec<String>) {
    let mut observations = Vec::new();
    let mut reasons = Vec::new();
    for probe in probes {
        if probe.underlay != TransportKind::Silvus {
            continue;
        }
        let calibration = config.radio.links.iter().find(|link| {
            transport(link.underlay) == probe.underlay
                && ((link.first == config.name && link.second == probe.peer)
                    || (link.second == config.name && link.first == probe.peer))
        });
        let Some(calibration) = calibration else {
            reasons.push(format!(
                "relay/{}/silvus: missing geometry and RF calibration",
                probe.peer
            ));
            continue;
        };
        match calibrated_observation(calibration, radios, probe, observed_at_ms) {
            Ok(observation) => observations.push(observation),
            Err(reason) => reasons.push(format!("relay/{}/silvus: {reason}", probe.peer)),
        }
    }
    (observations, reasons)
}

fn calibrated_observation(
    calibration: &CalibratedLinkConfig,
    radios: &[RadioApiObservation],
    probe: &PeerProbeObservation,
    observed_at_ms: u64,
) -> Result<RelayLinkObservation, &'static str> {
    let distance_m = calibration
        .distance_m
        .ok_or("missing distance calibration")?;
    let line_of_sight = calibration
        .line_of_sight
        .ok_or("missing line-of-sight evidence")?;
    let fresnel = calibration
        .fresnel_clearance_ratio
        .ok_or("missing Fresnel calibration")?;
    let snr_floor = calibration
        .snr_floor_db
        .ok_or("missing SNR floor calibration")?;
    let snr_ceiling = calibration
        .snr_ceiling_db
        .ok_or("missing SNR ceiling calibration")?;
    if snr_ceiling <= snr_floor {
        return Err("invalid SNR calibration range");
    }
    let sensitivity = calibration
        .receiver_sensitivity_dbm
        .ok_or("missing receiver sensitivity calibration")?;
    let energy_cost = calibration
        .energy_cost
        .ok_or("missing energy calibration")?;
    let first_radio = calibration
        .first_radio_node_id
        .ok_or("missing first radio node ID")?;
    let second_radio = calibration
        .second_radio_node_id
        .ok_or("missing second radio node ID")?;
    let first_evidence =
        radio_link(radios, first_radio, second_radio).ok_or("missing first-radio RF evidence")?;
    let second_evidence =
        radio_link(radios, second_radio, first_radio).ok_or("missing second-radio RF evidence")?;
    let latency_ms = probe.latency_ms.ok_or("missing latency measurement")? as f32;
    let goodput_bps = probe.goodput_bps.ok_or("missing goodput measurement")? as f32;
    let stability = probe
        .stability
        .ok_or("rolling stability is not established")? as f32;
    let snr = average(
        [first_evidence.snr_db, second_evidence.snr_db]
            .into_iter()
            .flatten(),
    )
    .ok_or("missing bidirectional SNR")?;
    let rssi = average(
        first_evidence
            .rssi_dbm
            .iter()
            .chain(&second_evidence.rssi_dbm)
            .copied(),
    )
    .ok_or("missing bidirectional RSSI")?;
    let signal_quality = ((snr - snr_floor) / (snr_ceiling - snr_floor)).clamp(0.0, 1.0) as f32;
    Ok(RelayLinkObservation {
        first: NodeId::from(calibration.first.clone()),
        second: NodeId::from(calibration.second.clone()),
        transport: TransportKind::Silvus,
        observed_at_ms,
        sample_window_ms: probe.sample_window_ms,
        bidirectional: true,
        available: probe.reachable,
        metrics: LinkMetrics {
            latency_ms,
            loss_ratio: probe.loss_ratio as f32,
            goodput_bps,
            signal_quality,
            stability,
            energy_cost,
        },
        geometry: LinkGeometry {
            distance_m,
            line_of_sight,
            fresnel_clearance_ratio: fresnel,
        },
        received_power_dbm: Some(rssi as f32),
        link_margin_db: Some((rssi - sensitivity) as f32),
    })
}

fn radio_link(
    radios: &[RadioApiObservation],
    local: u32,
    remote: u32,
) -> Option<&StreamCasterRfLink> {
    radios
        .iter()
        .find(|radio| {
            radio.api_fresh
                && radio
                    .effective_settings
                    .as_ref()
                    .and_then(|settings| settings.node_id)
                    == Some(local)
        })?
        .rf_links
        .iter()
        .find(|link| {
            (link.source_node_id == local && link.target_node_id == remote)
                || (link.source_node_id == remote && link.target_node_id == local)
        })
}

fn average(values: impl Iterator<Item = f64>) -> Option<f64> {
    let values = values.collect::<Vec<_>>();
    (!values.is_empty()).then(|| values.iter().sum::<f64>() / values.len() as f64)
}

fn read_credentials(path: &Path) -> anyhow::Result<Credentials> {
    validate_private_permissions(path)?;
    const MAX_CREDENTIAL_BYTES: u64 = 65_536;
    let metadata = std::fs::metadata(path)
        .with_context(|| format!("reading radio credential metadata {}", path.display()))?;
    anyhow::ensure!(
        metadata.len() <= MAX_CREDENTIAL_BYTES,
        "radio credentials exceed {MAX_CREDENTIAL_BYTES} bytes"
    );
    let encoded = std::fs::read(path)
        .with_context(|| format!("reading radio credentials {}", path.display()))?;
    serde_json::from_slice(&encoded)
        .with_context(|| format!("decoding radio credentials {}", path.display()))
}

#[cfg(unix)]
fn validate_private_permissions(path: &Path) -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let metadata = std::fs::metadata(path)?;
    anyhow::ensure!(
        metadata.is_file(),
        "radio credentials must be a regular file"
    );
    let mode = metadata.permissions().mode();
    anyhow::ensure!(mode & 0o077 == 0, "radio credentials must have mode 0600");
    Ok(())
}

#[cfg(not(unix))]
fn validate_private_permissions(_path: &Path) -> anyhow::Result<()> {
    Ok(())
}

fn spawn_probe_responder(address: SocketAddr) -> anyhow::Result<tokio::task::JoinHandle<()>> {
    let socket = std::net::UdpSocket::bind(address)
        .with_context(|| format!("binding link probe responder on {address}"))?;
    socket.set_nonblocking(true)?;
    let socket = UdpSocket::from_std(socket)?;
    Ok(tokio::spawn(async move {
        let mut buffer = vec![0_u8; 1_400];
        loop {
            let (length, source) = match socket.recv_from(&mut buffer).await {
                Ok(value) => value,
                Err(error) => {
                    eprintln!("Probe responder failed: {error}");
                    continue;
                }
            };
            if buffer[..length].starts_with(PROBE_MAGIC) {
                let _ = socket.send_to(&buffer[..length], source).await;
            }
        }
    }))
}

fn transport(underlay: Underlay) -> TransportKind {
    match underlay {
        Underlay::Silvus => TransportKind::Silvus,
        Underlay::Satellite => TransportKind::Satellite,
        Underlay::Ethernet => TransportKind::Ethernet,
        Underlay::Wifi => TransportKind::Wifi,
        Underlay::Other => TransportKind::Other,
    }
}

fn unix_time_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn bounded_udp_probe_measures_reachable_echo() {
        let reservation = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        let address = reservation.local_addr().unwrap();
        drop(reservation);
        let responder = spawn_probe_responder(address).unwrap();
        let observation = probe_peer(
            &PeerProbeConfig {
                peer: "ground".into(),
                underlay: Underlay::Silvus,
                address: address.to_string(),
                packets: 3,
                payload_bytes: 64,
            },
            200,
        )
        .await;
        responder.abort();
        assert!(observation.reachable);
        assert_eq!(observation.received_packets, 3);
        assert_eq!(observation.loss_ratio, 0.0);
        assert!(observation.goodput_bps.is_some());
    }

    #[test]
    fn passive_probes_run_without_enabling_radio_apis() {
        let mut radio = RadioConfig::default();
        assert!(!link_monitor_has_work(&radio));
        radio.probe_listen = Some("127.0.0.1:9200".parse().unwrap());
        assert!(link_monitor_has_work(&radio));
        assert!(!radio.enabled);
        assert!(radio.devices.is_empty());
    }

    #[test]
    fn missing_geometry_fails_relay_closed() {
        let probe = PeerProbeObservation {
            peer: "ground".into(),
            underlay: TransportKind::Silvus,
            observed_at_ms: 1,
            sample_window_ms: 10,
            sent_packets: 3,
            received_packets: 3,
            latency_ms: Some(1.0),
            loss_ratio: 0.0,
            goodput_bps: Some(10_000),
            stability: Some(1.0),
            reachable: true,
            error: None,
        };
        let config = CalibratedLinkConfig {
            first: "air".into(),
            second: "ground".into(),
            underlay: Underlay::Silvus,
            first_radio_node_id: Some(1),
            second_radio_node_id: Some(2),
            distance_m: None,
            line_of_sight: None,
            fresnel_clearance_ratio: None,
            snr_floor_db: None,
            snr_ceiling_db: None,
            receiver_sensitivity_dbm: None,
            energy_cost: None,
        };
        assert_eq!(
            calibrated_observation(&config, &[], &probe, 1),
            Err("missing distance calibration")
        );
    }

    #[tokio::test]
    async fn missing_radio_credentials_becomes_sanitized_degradation() {
        let directory = tempfile::tempdir().unwrap();
        let observation = observe_radio(
            &mesh_agent::config::RadioDeviceConfig {
                name: "air-radio".into(),
                base_url: "https://192.0.2.1".into(),
                credentials_file: directory.path().join("missing.json"),
                local_node_id: Some(1),
            },
            1,
        )
        .await;
        assert!(!observation.api_fresh);
        assert_eq!(observation.errors, vec!["credentials_unavailable"]);
        assert!(format!("{observation:?}")
            .find(directory.path().to_str().unwrap())
            .is_none());
    }
}
