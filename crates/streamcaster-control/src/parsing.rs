use super::*;

pub(super) fn parse_capabilities(
    raw: Value,
    model: Option<&str>,
    firmware_version: Option<String>,
    observed_at_ms: u64,
) -> Result<StreamCasterCapabilities, StreamCasterError> {
    let profiles = raw
        .as_array()
        .or_else(|| {
            raw.get("supported_frequency_profiles")
                .and_then(Value::as_array)
        })
        .or_else(|| raw.get("profiles").and_then(Value::as_array))
        .ok_or_else(|| {
            StreamCasterError::InvalidVendorShape(
                "supported_frequency_profiles array is missing".into(),
            )
        })?;
    let mut normalized = Vec::new();
    for profile in profiles {
        let profile = profile.as_object().ok_or_else(|| {
            StreamCasterError::InvalidVendorShape("frequency profile must be an object".into())
        })?;
        let antenna_mask = integer(profile, &["antenna_mask", "antennas"])?;
        let antenna_mask = u8::try_from(antenna_mask)
            .map_err(|_| StreamCasterError::InvalidVendorShape("antenna mask exceeds u8".into()))?;
        let bandwidths =
            profile_bandwidths(value(profile, &["bandwidth_mhz", "bw", "bandwidth"])?)?;
        let frequencies =
            profile_frequencies(value(profile, &["frequencies", "frequency", "freq"])?)?;
        for bandwidth_mhz in &bandwidths {
            for center_frequency_mhz in &frequencies {
                normalized.push(StreamCasterFrequencyProfile {
                    center_frequency_mhz: *center_frequency_mhz,
                    bandwidth_mhz: *bandwidth_mhz,
                    antenna_mask,
                });
            }
        }
    }
    normalized.sort_by(|left, right| {
        left.center_frequency_mhz
            .total_cmp(&right.center_frequency_mhz)
            .then_with(|| {
                bandwidth_number(left.bandwidth_mhz)
                    .total_cmp(&bandwidth_number(right.bandwidth_mhz))
            })
            .then_with(|| left.antenna_mask.cmp(&right.antenna_mask))
    });
    normalized.dedup();
    Ok(StreamCasterCapabilities {
        observed_at_ms,
        model: model.and_then(parse_model),
        firmware_version,
        supported_frequency_profiles: normalized,
        scheduled_activation_supported: false,
        dual_profile_supported: false,
    })
}

pub(super) fn parse_effective_settings(
    raw: Value,
    observed_at_ms: u64,
) -> Result<StreamCasterEffectiveSettings, StreamCasterError> {
    let root = normalize_settings(raw)?;
    let root = &root;
    Ok(StreamCasterEffectiveSettings {
        observed_at_ms,
        node_id: integer_optional(root, &["nodeid", "node_id"])
            .map(u32::try_from)
            .transpose()
            .map_err(|_| StreamCasterError::InvalidVendorShape("node ID exceeds u32".into()))?,
        system_name: string_optional(root, &["system_name", "node_label", "name"]),
        network_id: string(root, &["nw_name", "network_id", "networkid"])?,
        center_frequency_mhz: number(root, &["center_frequency_mhz", "freq", "frequency"])?,
        bandwidth_mhz: bandwidth(value(root, &["bandwidth_mhz", "bw", "bandwidth"])?)?,
        link_distance_m: u32::try_from(integer(
            root,
            &["max_link_distance", "link_distance_m", "link_distance"],
        )?)
        .map_err(|_| StreamCasterError::InvalidVendorShape("link distance exceeds u32".into()))?,
        antenna_mask: u8::try_from(integer(root, &["tx_ant_mask", "antenna_mask", "antennas"])?)
            .map_err(|_| StreamCasterError::InvalidVendorShape("antenna mask exceeds u8".into()))?,
        max_power_enabled: integer_optional(root, &["enable_max_power"]).map(|value| value != 0),
        transmit_power_dbm_per_port: integer_optional(
            root,
            &["transmit_power_dbm_per_port", "power_dBm"],
        )
        .map(u8::try_from)
        .transpose()
        .map_err(|_| StreamCasterError::InvalidVendorShape("power exceeds u8".into()))?,
    })
}

pub(super) fn normalize_settings(
    raw: Value,
) -> Result<serde_json::Map<String, Value>, StreamCasterError> {
    if let Some(object) = raw.as_object() {
        return Ok(object.clone());
    }
    let values = raw.as_array().ok_or_else(|| {
        StreamCasterError::InvalidVendorShape(
            "print_all_settings must return alternating command/value entries".into(),
        )
    })?;
    if values.len() % 2 != 0 {
        return Err(StreamCasterError::InvalidVendorShape(
            "print_all_settings returned an unmatched command/value entry".into(),
        ));
    }
    const ALLOWED: &[&str] = &[
        "nodeid",
        "system_name",
        "node_label",
        "nw_name",
        "freq",
        "bw",
        "max_link_distance",
        "tx_ant_mask",
        "rx_ant_mask",
        "power_dBm",
        "enable_max_power",
        "mcs",
        "routing_proto",
        "version",
        "model",
    ];
    let mut result = serde_json::Map::new();
    for pair in values.chunks_exact(2) {
        let Some(command) = pair[0].as_str() else {
            continue;
        };
        if !ALLOWED.contains(&command) {
            continue;
        }
        let value = pair[1]
            .as_array()
            .and_then(|entries| entries.first())
            .cloned()
            .unwrap_or(Value::Null);
        result.insert(command.to_owned(), value);
    }
    Ok(result)
}

pub(super) fn scalar_string(value: Value) -> Option<String> {
    match value {
        Value::String(value) => Some(value),
        Value::Array(values) => values.into_iter().next().and_then(scalar_string),
        Value::Number(value) => Some(value.to_string()),
        _ => None,
    }
}

pub(super) fn parse_model(value: &str) -> Option<mesh_core::StreamCasterModel> {
    let compact = value.to_ascii_lowercase().replace(['-', '_', ' '], "");
    use mesh_core::StreamCasterModel as Model;
    if compact.contains("sl5205") {
        Some(Model::Sl5205)
    } else if compact.contains("sl5210") {
        Some(Model::Sl5210)
    } else if compact.contains("sl5220") {
        Some(Model::Sl5220)
    } else if compact.contains("sl5200") {
        Some(Model::Sl5200)
    } else if compact.contains("sc4400x") {
        Some(Model::Sc4400X)
    } else if compact.contains("sc4400e") {
        Some(Model::Sc4400E)
    } else if compact.contains("sc4400") {
        Some(Model::Sc4400)
    } else if compact.contains("sc4200ep") {
        Some(Model::Sc4200Ep)
    } else if compact.contains("sc4200") {
        Some(Model::Sc4200)
    } else if compact.contains("sl4200") {
        Some(Model::Sl4200)
    } else {
        None
    }
}

pub(super) fn profile_bandwidths(
    value: &Value,
) -> Result<Vec<ChannelBandwidthMhz>, StreamCasterError> {
    if parse_number(value)? == -1.0 {
        return Ok(vec![ChannelBandwidthMhz::Mhz5, ChannelBandwidthMhz::Mhz20]);
    }
    Ok(vec![bandwidth(value)?])
}

pub(super) fn profile_frequencies(value: &Value) -> Result<Vec<f64>, StreamCasterError> {
    let entries: Vec<&str> = match value {
        Value::Array(values) => values.iter().filter_map(Value::as_str).collect(),
        Value::String(value) => vec![value.as_str()],
        _ => Vec::new(),
    };
    if entries.is_empty() {
        return Err(StreamCasterError::InvalidVendorShape(
            "frequency list is empty".into(),
        ));
    }
    let mut result = Vec::new();
    for entry in entries {
        let parts: Vec<_> = entry.split(':').collect();
        match parts.as_slice() {
            [single] => {
                result.push(single.parse().map_err(|_| {
                    StreamCasterError::InvalidVendorShape("invalid frequency".into())
                })?)
            }
            [start, step, end] => {
                let mut current: f64 = start.parse().map_err(|_| {
                    StreamCasterError::InvalidVendorShape("invalid frequency range".into())
                })?;
                let step: f64 = step.parse().map_err(|_| {
                    StreamCasterError::InvalidVendorShape("invalid frequency step".into())
                })?;
                let end: f64 = end.parse().map_err(|_| {
                    StreamCasterError::InvalidVendorShape("invalid frequency range".into())
                })?;
                if step <= 0.0 || current > end {
                    return Err(StreamCasterError::InvalidVendorShape(
                        "invalid frequency range".into(),
                    ));
                }
                while current <= end + 0.000_1 {
                    result.push(current);
                    current += step;
                }
            }
            _ => {
                return Err(StreamCasterError::InvalidVendorShape(
                    "invalid frequency profile".into(),
                ));
            }
        }
    }
    Ok(result)
}

pub(super) fn bandwidth_number(value: ChannelBandwidthMhz) -> f64 {
    match value {
        ChannelBandwidthMhz::Mhz1_25 => 1.25,
        ChannelBandwidthMhz::Mhz2_5 => 2.5,
        ChannelBandwidthMhz::Mhz5 => 5.0,
        ChannelBandwidthMhz::Mhz10 => 10.0,
        ChannelBandwidthMhz::Mhz20 => 20.0,
    }
}

pub(super) fn value<'a>(
    map: &'a serde_json::Map<String, Value>,
    names: &[&str],
) -> Result<&'a Value, StreamCasterError> {
    names
        .iter()
        .find_map(|name| map.get(*name))
        .ok_or_else(|| StreamCasterError::InvalidVendorShape(format!("missing {}", names[0])))
}

pub(super) fn number(
    map: &serde_json::Map<String, Value>,
    names: &[&str],
) -> Result<f64, StreamCasterError> {
    parse_number(value(map, names)?)
}

pub(super) fn parse_number(value: &Value) -> Result<f64, StreamCasterError> {
    let parsed = value
        .as_f64()
        .or_else(|| value.as_str().and_then(|text| text.trim().parse().ok()))
        .filter(|number: &f64| number.is_finite())
        .ok_or_else(|| StreamCasterError::InvalidVendorShape("expected finite number".into()))?;
    Ok(parsed)
}

pub(super) fn integer(
    map: &serde_json::Map<String, Value>,
    names: &[&str],
) -> Result<u64, StreamCasterError> {
    integer_value(value(map, names)?)
}

pub(super) fn integer_optional(
    map: &serde_json::Map<String, Value>,
    names: &[&str],
) -> Option<u64> {
    names
        .iter()
        .find_map(|name| map.get(*name))
        .and_then(|value| integer_value(value).ok())
}

pub(super) fn integer_value(value: &Value) -> Result<u64, StreamCasterError> {
    value
        .as_u64()
        .or_else(|| value.as_str().and_then(|text| text.trim().parse().ok()))
        .ok_or_else(|| StreamCasterError::InvalidVendorShape("expected unsigned integer".into()))
}

pub(super) fn string(
    map: &serde_json::Map<String, Value>,
    names: &[&str],
) -> Result<String, StreamCasterError> {
    string_optional(map, names)
        .ok_or_else(|| StreamCasterError::InvalidVendorShape(format!("missing {}", names[0])))
}

pub(super) fn string_optional(
    map: &serde_json::Map<String, Value>,
    names: &[&str],
) -> Option<String> {
    names
        .iter()
        .find_map(|name| map.get(*name))
        .and_then(Value::as_str)
        .map(str::to_owned)
}

pub(super) fn bandwidth(value: &Value) -> Result<ChannelBandwidthMhz, StreamCasterError> {
    let number = parse_number(value)?;
    match number {
        value if (value - 1.25).abs() < 0.001 => Ok(ChannelBandwidthMhz::Mhz1_25),
        value if (value - 2.5).abs() < 0.001 => Ok(ChannelBandwidthMhz::Mhz2_5),
        value if (value - 5.0).abs() < 0.001 => Ok(ChannelBandwidthMhz::Mhz5),
        value if (value - 10.0).abs() < 0.001 => Ok(ChannelBandwidthMhz::Mhz10),
        value if (value - 20.0).abs() < 0.001 => Ok(ChannelBandwidthMhz::Mhz20),
        _ => Err(StreamCasterError::InvalidVendorShape(format!(
            "unsupported bandwidth {number}"
        ))),
    }
}
