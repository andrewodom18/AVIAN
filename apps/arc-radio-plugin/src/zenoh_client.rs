/// Opens an explicitly addressed client without multicast or gossip discovery.
pub async fn open(endpoint: &str) -> anyhow::Result<zenoh::Session> {
    let mut config = zenoh::Config::default();
    config
        .insert_json5("mode", r#""client""#)
        .map_err(|error| anyhow::anyhow!("zenoh mode: {error}"))?;
    config
        .insert_json5("connect/endpoints", &serde_json::to_string(&[endpoint])?)
        .map_err(|error| anyhow::anyhow!("zenoh endpoint: {error}"))?;
    for setting in ["scouting/multicast/enabled", "scouting/gossip/enabled"] {
        config
            .insert_json5(setting, "false")
            .map_err(|error| anyhow::anyhow!("zenoh discovery: {error}"))?;
    }
    zenoh::open(config)
        .await
        .map_err(|error| anyhow::anyhow!("opening Zenoh: {error}"))
}
