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
