use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{bail, Context};
use clap::Args;
use mesh_core::{NodeId, RadioDeviceObservation};
use trellisware_control::{ClientIdentity, HttpsTncAgentTransport, TrellisWareReader};
use zeroize::Zeroizing;

const RADIO_OBSERVATIONS_TOPIC: &str = "local/link/radio/observations/v1";

#[derive(Debug, Args)]
pub struct TrellisWareProbeArgs {
    /// TW-950 management URL, for example https://10.1.0.11.
    #[arg(long)]
    radio_url: String,
    /// Stable ARC/AVIAN node key for this physical radio.
    #[arg(long)]
    source: String,
    /// Combined PEM client certificate and private key when the radio requires mTLS.
    #[arg(long, conflicts_with = "client_identity_pkcs12")]
    client_identity_pem: Option<PathBuf>,
    /// PKCS#12 client identity when the radio requires mTLS.
    #[arg(long, conflicts_with = "client_identity_pem")]
    client_identity_pkcs12: Option<PathBuf>,
    /// File containing the PKCS#12 password. Omit for a blank password.
    #[arg(long, requires = "client_identity_pkcs12")]
    client_identity_pkcs12_password_file: Option<PathBuf>,
    /// PEM CA certificate used to validate the radio's HTTPS certificate.
    #[arg(long)]
    ca_certificate_pem: Option<PathBuf>,
    /// Lab-only override for a self-signed radio certificate.
    #[arg(long, default_value_t = false)]
    accept_invalid_server_certificate: bool,
    /// Optional ARC comms endpoint. When set, publish observations to the live UI.
    #[arg(long)]
    zenoh_endpoint: Option<String>,
    /// Continue polling instead of performing one read.
    #[arg(long, default_value_t = false)]
    watch: bool,
    /// Poll interval for --watch.
    #[arg(long, default_value_t = 5)]
    interval_seconds: u64,
    /// Optional JSON output path for the latest observation.
    #[arg(long)]
    output: Option<PathBuf>,
}

pub async fn run(args: &TrellisWareProbeArgs) -> anyhow::Result<()> {
    if args.source.trim().is_empty() {
        bail!("--source cannot be empty");
    }
    if args.interval_seconds == 0 {
        bail!("--interval-seconds must be positive");
    }
    let identity_pem = read_sensitive_optional(args.client_identity_pem.as_deref())?;
    let identity_pkcs12 = read_sensitive_optional(args.client_identity_pkcs12.as_deref())?;
    let identity_pkcs12_password =
        read_password(args.client_identity_pkcs12_password_file.as_deref())?;
    let identity = match (identity_pem.as_deref(), identity_pkcs12.as_deref()) {
        (Some(pem), None) => Some(ClientIdentity::Pem(pem)),
        (None, Some(der)) => Some(ClientIdentity::Pkcs12 {
            der,
            password: &identity_pkcs12_password,
        }),
        (None, None) => None,
        (Some(_), Some(_)) => unreachable!("clap rejects conflicting identity inputs"),
    };
    let ca = read_optional(args.ca_certificate_pem.as_deref())?;
    let transport = HttpsTncAgentTransport::new_with_identity(
        &args.radio_url,
        identity,
        ca.as_deref(),
        args.accept_invalid_server_certificate,
    )
    .context("creating read-only TW-950 HTTPS client")?;
    let reader = TrellisWareReader::new(transport);
    let session = match args.zenoh_endpoint.as_deref() {
        Some(endpoint) => Some(open_zenoh(endpoint).await?),
        None => None,
    };

    loop {
        let observation = match reader
            .read_observation(
                NodeId::from(args.source.clone()),
                management_ip(&args.radio_url),
                now_unix_ms(),
                false,
            )
            .await
            .context("reading TW-950 observation")
        {
            Ok(observation) => observation,
            Err(error) if args.watch => {
                eprintln!("TW-950 probe iteration failed; retrying: {error:#}");
                tokio::time::sleep(Duration::from_secs(args.interval_seconds)).await;
                continue;
            }
            Err(error) => return Err(error),
        };
        if let Err(error) = emit(&observation, args.output.as_deref()) {
            if !args.watch {
                return Err(error);
            }
            eprintln!("TW-950 probe output failed; retrying: {error:#}");
            tokio::time::sleep(Duration::from_secs(args.interval_seconds)).await;
            continue;
        }
        if let Some(session) = session.as_ref() {
            let publish = session
                .put(RADIO_OBSERVATIONS_TOPIC, serde_json::to_vec(&observation)?)
                .await
                .map_err(|error| anyhow::anyhow!("publishing TW-950 observation: {error}"));
            if let Err(error) = publish {
                if !args.watch {
                    return Err(error);
                }
                eprintln!("TW-950 probe publish failed; retrying: {error:#}");
            }
        }
        if !args.watch {
            break;
        }
        tokio::time::sleep(Duration::from_secs(args.interval_seconds)).await;
    }
    Ok(())
}

fn emit(observation: &RadioDeviceObservation, output: Option<&Path>) -> anyhow::Result<()> {
    let encoded = serde_json::to_string_pretty(observation)?;
    if let Some(path) = output {
        atomic_write(path, format!("{encoded}\n").as_bytes())?;
    } else {
        println!("{encoded}");
    }
    Ok(())
}

fn atomic_write(path: &Path, contents: &[u8]) -> anyhow::Result<()> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let mut temporary = tempfile::NamedTempFile::new_in(parent)
        .with_context(|| format!("creating temporary output beside {}", path.display()))?;
    std::io::Write::write_all(&mut temporary, contents)
        .with_context(|| format!("writing temporary output for {}", path.display()))?;
    temporary
        .persist(path)
        .map_err(|error| error.error)
        .with_context(|| format!("replacing {} atomically", path.display()))?;
    Ok(())
}

fn read_optional(path: Option<&Path>) -> anyhow::Result<Option<Vec<u8>>> {
    path.map(|path| std::fs::read(path).with_context(|| format!("reading {}", path.display())))
        .transpose()
}

fn read_sensitive_optional(path: Option<&Path>) -> anyhow::Result<Option<Zeroizing<Vec<u8>>>> {
    path.map(|path| {
        std::fs::read(path)
            .map(Zeroizing::new)
            .context("reading client identity file")
    })
    .transpose()
}

fn read_password(path: Option<&Path>) -> anyhow::Result<Zeroizing<String>> {
    let Some(encoded) = read_sensitive_optional(path)? else {
        return Ok(Zeroizing::new(String::new()));
    };
    let mut password = String::from_utf8(encoded.to_vec())
        .map(Zeroizing::new)
        .context("client identity password file must contain UTF-8 text")?;
    while password.ends_with(['\r', '\n']) {
        password.pop();
    }
    Ok(password)
}

fn management_ip(url: &str) -> Option<String> {
    reqwest::Url::parse(url)
        .ok()?
        .host_str()
        .map(|host| host.trim_matches(['[', ']']).to_owned())
}

async fn open_zenoh(endpoint: &str) -> anyhow::Result<zenoh::Session> {
    let mut config = zenoh::Config::default();
    config
        .insert_json5("mode", r#""client""#)
        .map_err(|error| anyhow::anyhow!("zenoh mode: {error}"))?;
    config
        .insert_json5("connect/endpoints", &format!(r#"["{endpoint}"]"#))
        .map_err(|error| anyhow::anyhow!("zenoh endpoint: {error}"))?;
    config
        .insert_json5("scouting/multicast/enabled", "false")
        .map_err(|error| anyhow::anyhow!("zenoh multicast: {error}"))?;
    config
        .insert_json5("scouting/gossip/enabled", "false")
        .map_err(|error| anyhow::anyhow!("zenoh gossip: {error}"))?;
    zenoh::open(config)
        .await
        .map_err(|error| anyhow::anyhow!("opening Zenoh: {error}"))
}

fn now_unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn extracts_management_host_without_credentials_or_path() {
        assert_eq!(
            management_ip("https://10.1.0.11/agent/"),
            Some("10.1.0.11".into())
        );
    }

    #[test]
    fn extracts_bracketed_ipv6_management_host() {
        assert_eq!(
            management_ip("https://[fe80::21e:3fff:fe20:9a10]:8443/agent/"),
            Some("[fe80::21e:3fff:fe20:9a10]".trim_matches(['[', ']']).into())
        );
    }

    #[test]
    fn omitted_pkcs12_password_is_blank() {
        assert_eq!(read_password(None).unwrap().as_str(), "");
    }

    #[test]
    fn password_file_removes_only_line_endings() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("password.txt");
        std::fs::write(&path, b" leading and trailing spaces \r\n").unwrap();
        assert_eq!(
            read_password(Some(&path)).unwrap().as_str(),
            " leading and trailing spaces "
        );
    }
}
