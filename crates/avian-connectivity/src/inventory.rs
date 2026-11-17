//! Read-only interface inspection. Platform adapters never change host routes here.
use crate::{ConnectivityError, Metering, Underlay};
use serde::Deserialize;
use std::{collections::BTreeMap, net::IpAddr, process::Stdio, time::Duration};
use tokio::{io::AsyncReadExt, process::Command, time::timeout};

pub async fn collect_interfaces() -> Result<Vec<Underlay>, ConnectivityError> {
    #[cfg(target_os = "linux")]
    let (program, arguments) = ("/usr/sbin/ip", vec!["-j", "address", "show"]);
    #[cfg(target_os = "macos")]
    let (program, arguments) = ("/sbin/ifconfig", vec!["-a"]);
    #[cfg(target_os = "windows")]
    let (program, arguments) = (
        "powershell.exe",
        vec![
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "Get-NetIPAddress | Select-Object InterfaceAlias,IPAddress,AddressState | ConvertTo-Json -Compress",
        ],
    );
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    return Err(ConnectivityError::InventoryUnavailable);
    #[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
    read_inventory(program, &arguments, parse_platform).await
}

fn parse_platform(text: &str) -> Result<Vec<Underlay>, ConnectivityError> {
    #[cfg(target_os = "linux")]
    {
        parse_linux(text)
    }
    #[cfg(target_os = "macos")]
    {
        parse_macos(text)
    }
    #[cfg(target_os = "windows")]
    {
        parse_windows(text)
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    {
        let _ = text;
        Err(ConnectivityError::InventoryUnavailable)
    }
}

pub async fn collect_namespace_interfaces(name: &str) -> Result<Vec<Underlay>, ConnectivityError> {
    if !cfg!(target_os = "linux")
        || name.is_empty()
        || name.len() > 63
        || !name
            .bytes()
            .all(|value| value.is_ascii_alphanumeric() || matches!(value, b'-' | b'_'))
    {
        return Err(ConnectivityError::InventoryUnavailable);
    }
    read_inventory(
        "/usr/sbin/ip",
        &[
            "netns",
            "exec",
            name,
            "/usr/sbin/ip",
            "-j",
            "address",
            "show",
        ],
        parse_linux,
    )
    .await
}

async fn read_inventory(
    program: &str,
    arguments: &[&str],
    parse: fn(&str) -> Result<Vec<Underlay>, ConnectivityError>,
) -> Result<Vec<Underlay>, ConnectivityError> {
    let mut child = Command::new(program)
        .args(arguments)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(|_| ConnectivityError::InventoryUnavailable)?;
    let stdout = child
        .stdout
        .take()
        .ok_or(ConnectivityError::InventoryUnavailable)?;
    let reading = async {
        let mut bytes = Vec::new();
        stdout
            .take(1024 * 1024 + 1)
            .read_to_end(&mut bytes)
            .await
            .map_err(|_| ConnectivityError::InventoryUnavailable)?;
        if bytes.len() > 1024 * 1024
            || !child
                .wait()
                .await
                .map_err(|_| ConnectivityError::InventoryUnavailable)?
                .success()
        {
            return Err(ConnectivityError::InventoryUnavailable);
        }
        parse(std::str::from_utf8(&bytes).map_err(|_| ConnectivityError::InventoryUnavailable)?)
    };
    timeout(Duration::from_secs(3), reading)
        .await
        .map_err(|_| ConnectivityError::InventoryUnavailable)?
}

#[derive(Deserialize)]
struct LinuxInterface {
    ifname: String,
    flags: Vec<String>,
    addr_info: Vec<LinuxAddress>,
}
#[derive(Deserialize)]
struct LinuxAddress {
    local: IpAddr,
}

pub fn parse_linux(text: &str) -> Result<Vec<Underlay>, ConnectivityError> {
    let records: Vec<LinuxInterface> =
        serde_json::from_str(text).map_err(|_| ConnectivityError::InventoryUnavailable)?;
    records
        .into_iter()
        .map(|record| {
            let observation = Underlay {
                interface_name: record.ifname,
                addresses: record
                    .addr_info
                    .into_iter()
                    .map(|address| address.local)
                    .collect(),
                available: record.flags.iter().any(|flag| flag == "UP")
                    && record.flags.iter().any(|flag| flag == "LOWER_UP"),
                metering: Metering::Unknown,
            };
            observation.validate()?;
            Ok(observation)
        })
        .collect()
}

pub fn parse_macos(text: &str) -> Result<Vec<Underlay>, ConnectivityError> {
    let mut records: Vec<Underlay> = Vec::new();
    for line in text.lines() {
        if !line.starts_with(char::is_whitespace) {
            let Some((name, details)) = line.split_once(':') else {
                continue;
            };
            records.push(Underlay {
                interface_name: name.to_owned(),
                addresses: Vec::new(),
                available: details.split(['<', '>', ',']).any(|flag| flag == "UP"),
                metering: Metering::Unknown,
            });
        } else if let Some(record) = records.last_mut() {
            let mut fields = line.split_whitespace();
            match (fields.next(), fields.next()) {
                (Some("inet" | "inet6"), Some(address)) => {
                    let address = address
                        .split('%')
                        .next()
                        .unwrap_or("")
                        .parse()
                        .map_err(|_| ConnectivityError::InventoryUnavailable)?;
                    record.addresses.push(address);
                }
                (Some("status:"), Some("inactive")) => record.available = false,
                _ => {}
            }
        }
    }
    for record in &records {
        record.validate()?;
    }
    Ok(records)
}

pub fn parse_windows(text: &str) -> Result<Vec<Underlay>, ConnectivityError> {
    #[derive(Deserialize)]
    #[serde(rename_all = "PascalCase")]
    struct Address {
        interface_alias: String,
        #[serde(rename = "IPAddress")]
        ip_address: IpAddr,
        address_state: serde_json::Value,
    }
    let value: serde_json::Value =
        serde_json::from_str(text).map_err(|_| ConnectivityError::InventoryUnavailable)?;
    let rows = match value {
        serde_json::Value::Array(rows) => rows,
        serde_json::Value::Null => Vec::new(),
        row => vec![row],
    };
    let mut records = BTreeMap::new();
    for row in rows {
        let address: Address =
            serde_json::from_value(row).map_err(|_| ConnectivityError::InventoryUnavailable)?;
        let preferred = address.address_state.as_u64() == Some(4)
            || address.address_state.as_str() == Some("Preferred");
        let record = records
            .entry(address.interface_alias.clone())
            .or_insert(Underlay {
                interface_name: address.interface_alias,
                addresses: Vec::new(),
                available: false,
                metering: Metering::Unknown,
            });
        if preferred {
            record.available = true;
            record.addresses.push(address.ip_address);
        }
    }
    let result: Vec<Underlay> = records.into_values().collect();
    for record in &result {
        record.validate()?;
    }
    Ok(result)
}
