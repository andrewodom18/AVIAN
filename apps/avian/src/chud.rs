use std::time::Duration;

use anyhow::{Context, Result, bail};
use reqwest::{
    Client, Url,
    header::{HeaderMap, HeaderValue},
};
use serde::{Deserialize, Serialize};

use crate::configuration::{ChudConfiguration, read_secret};

#[derive(Clone)]
pub struct ChudClient {
    client: Client,
    inventory_url: Url,
    configuration_allowed: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct RadioDevice {
    pub mac: String,
    pub radio_type: String,
    pub label: String,
    pub state: String,
    #[serde(default)]
    pub reach_addr: Option<String>,
    #[serde(default)]
    pub reach_iface: Option<String>,
    pub driver_available: bool,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct RadioInventory {
    #[serde(default)]
    pub devices: Vec<RadioDevice>,
}

impl ChudClient {
    pub fn new(configuration: &ChudConfiguration) -> Result<Self> {
        let mut base_url = Url::parse(&configuration.url).context("parse CHUD URL")?;
        let loopback = base_url.host_str().is_some_and(|host| {
            host == "localhost"
                || host
                    .trim_matches(['[', ']'])
                    .parse::<std::net::IpAddr>()
                    .is_ok_and(|address| address.is_loopback())
        });
        if base_url.scheme() != "https" && !(base_url.scheme() == "http" && loopback) {
            bail!("CHUD requires HTTPS except on a loopback address");
        }
        if !base_url.username().is_empty()
            || base_url.password().is_some()
            || base_url.query().is_some()
            || base_url.fragment().is_some()
            || base_url.path() != "/"
        {
            bail!("CHUD URL must contain only a scheme, host, and optional port");
        }
        let mut headers = HeaderMap::new();
        let mut credential = HeaderValue::from_str(&read_secret(&configuration.api_key_file)?)
            .context("invalid CHUD API key")?;
        credential.set_sensitive(true);
        headers.insert("X-API-Key", credential);
        let mut builder = Client::builder()
            .no_proxy()
            .default_headers(headers)
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(5));
        if let Some(certificate_file) = &configuration.certificate_file {
            let certificate = std::fs::read(certificate_file).context("read CHUD certificate")?;
            builder = builder.add_root_certificate(reqwest::Certificate::from_pem(&certificate)?);
        }
        base_url.set_path("/api/radio/devices");
        Ok(Self {
            client: builder.build()?,
            inventory_url: base_url,
            configuration_allowed: configuration.allow_configuration,
        })
    }

    pub async fn inventory(&self) -> Result<RadioInventory> {
        let mut response = self
            .client
            .get(self.inventory_url.clone())
            .send()
            .await
            .context("CHUD inventory request failed")?;
        if !response.status().is_success() {
            bail!(
                "CHUD inventory returned HTTP {}",
                response.status().as_u16()
            );
        }
        // This bounds one upstream response, not the number of network members.
        let maximum_response_bytes = 8 * 1024 * 1024;
        let mut body = Vec::new();
        while let Some(chunk) = response.chunk().await? {
            if body.len() + chunk.len() > maximum_response_bytes {
                bail!("CHUD inventory response exceeded the local response budget");
            }
            body.extend_from_slice(&chunk);
        }
        serde_json::from_slice(&body).context("invalid CHUD inventory response")
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RadioParameter {
    pub value: serde_json::Value,
    pub block: String,
}
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RadioApply {
    pub mac: String,
    pub desired: std::collections::BTreeMap<String, RadioParameter>,
    pub acknowledge_disruption: bool,
}
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RadioConfirm {
    pub operation_id: String,
    pub acknowledge_persistence: bool,
}
impl ChudClient {
    pub fn configuration_allowed(&self) -> bool {
        self.configuration_allowed
    }
    fn endpoint(&self, path: &str) -> Url {
        let mut endpoint = self.inventory_url.clone();
        endpoint.set_path(path);
        endpoint
    }
    pub async fn apply(&self, request: RadioApply) -> Result<serde_json::Value> {
        if !self.configuration_allowed || !request.acknowledge_disruption {
            bail!(
                "radio changes require enabled configuration and explicit disruption acknowledgement"
            );
        }
        validate_mac(&request.mac)?;
        if request.desired.is_empty()
            || request.desired.len() > 128
            || request.desired.iter().any(|(name, parameter)| {
                name.is_empty()
                    || name.len() > 128
                    || name.chars().any(char::is_control)
                    || parameter.block.len() > 128
                    || parameter.block.chars().any(char::is_control)
            })
        {
            bail!("invalid radio parameters");
        }
        let body =
            serde_json::json!({"mac":request.mac,"desired":request.desired,"defer_persist":true});
        if serde_json::to_vec(&body)?.len() > 64 * 1024 {
            bail!("radio request exceeded the local byte budget");
        }
        // An ambiguous timeout is never retried: CHUD may already have started the
        // transaction. The caller can inspect retained operations to recover its ID.
        read_response(
            self.client
                .post(self.endpoint("/api/radio/apply"))
                .json(&body)
                .send()
                .await?,
        )
        .await
    }
    pub async fn confirm(&self, request: RadioConfirm) -> Result<serde_json::Value> {
        if !self.configuration_allowed
            || !request.acknowledge_persistence
            || request.operation_id.is_empty()
            || request.operation_id.len() > 128
            || request.operation_id.chars().any(char::is_control)
        {
            bail!("radio persistence requires explicit confirmation");
        }
        read_response(
            self.client
                .post(self.endpoint("/api/radio/confirm"))
                .json(&serde_json::json!({"operation_id":request.operation_id}))
                .send()
                .await?,
        )
        .await
    }
    pub async fn operations(&self, mac: Option<&str>) -> Result<serde_json::Value> {
        let mut endpoint = self.endpoint("/api/radio/operations");
        if let Some(mac) = mac {
            validate_mac(mac)?;
            endpoint.query_pairs_mut().append_pair("mac", mac);
        }
        read_response(self.client.get(endpoint).send().await?).await
    }
    pub async fn snapshot(&self, mac: &str) -> Result<serde_json::Value> {
        validate_mac(mac)?;
        let mut endpoint = self.endpoint("/api/radio/snapshot");
        endpoint.query_pairs_mut().append_pair("mac", mac);
        let mut snapshot = read_response(self.client.get(endpoint).send().await?).await?;
        if let Some(parameters) = snapshot
            .get_mut("config")
            .and_then(serde_json::Value::as_object_mut)
        {
            for parameter in parameters.values_mut() {
                if parameter
                    .pointer("/meta/sensitive")
                    .and_then(serde_json::Value::as_bool)
                    == Some(true)
                {
                    parameter["value"] = serde_json::json!("<redacted>");
                }
            }
        }
        Ok(snapshot)
    }
}
fn validate_mac(mac: &str) -> Result<()> {
    let octets: Vec<_> = mac.split(':').collect();
    if octets.len() != 6
        || octets
            .iter()
            .any(|octet| octet.len() != 2 || !octet.bytes().all(|byte| byte.is_ascii_hexdigit()))
    {
        bail!("invalid radio MAC address");
    }
    Ok(())
}
async fn read_response(mut response: reqwest::Response) -> Result<serde_json::Value> {
    if !response.status().is_success() {
        bail!(
            "CHUD operation returned HTTP {}",
            response.status().as_u16()
        );
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        if bytes.len().saturating_add(chunk.len()) > 8 * 1024 * 1024 {
            bail!("CHUD response exceeded the local byte budget");
        }
        bytes.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&bytes).context("invalid CHUD operation response")
}
