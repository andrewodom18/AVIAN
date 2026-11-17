use std::{
    fs,
    net::SocketAddr,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Configuration {
    pub listen: SocketAddr,
    pub token_file: PathBuf,
    pub membership_file: PathBuf,
    pub trust_root_file: PathBuf,
    #[serde(default)]
    pub chud: Option<ChudConfiguration>,
    #[serde(default)]
    pub runtime: Option<avian_runtime::RuntimeConfiguration>,
    #[serde(default)]
    pub services: Option<crate::services::ServiceConfiguration>,
    #[serde(default)]
    pub service_exchange: Option<crate::service_exchange::ServiceExchangeConfiguration>,
    #[serde(default)]
    pub service_limits: Option<avian_services::Limits>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ChudConfiguration {
    #[serde(default)]
    pub allow_configuration: bool,
    pub url: String,
    pub api_key_file: PathBuf,
    pub certificate_file: Option<PathBuf>,
}

impl Configuration {
    pub fn load(path: &Path) -> Result<Self> {
        let text = fs::read_to_string(path).context("read configuration")?;
        let mut configuration: Self = toml::from_str(&text).context("parse configuration")?;
        if !configuration.listen.ip().is_loopback() {
            bail!("the local API must listen on a loopback address");
        }
        let directory = path.parent().unwrap_or_else(|| Path::new("."));
        for relative_path in [
            &mut configuration.token_file,
            &mut configuration.membership_file,
            &mut configuration.trust_root_file,
        ] {
            if relative_path.is_relative() {
                *relative_path = directory.join(&*relative_path);
            }
        }
        if let Some(chud) = &mut configuration.chud {
            if chud.api_key_file.is_relative() {
                chud.api_key_file = directory.join(&chud.api_key_file);
            }
            if let Some(certificate) = &mut chud.certificate_file
                && certificate.is_relative()
            {
                *certificate = directory.join(&*certificate);
            }
        }
        if let Some(exchange) = &configuration.service_exchange {
            exchange.validate()?;
            if configuration.services.is_none() {
                bail!("service exchange requires a service registry");
            }
            if let Some(runtime) = &configuration.runtime
                && exchange.listen.ip() != std::net::IpAddr::V4(runtime.overlay_address)
            {
                bail!("service exchange must bind the configured overlay address");
            }
        }
        if let Some(services) = &mut configuration.services {
            if services.state_file.is_relative() {
                services.state_file = directory.join(&services.state_file);
            }
            if let Some(identity) = &mut services.identity_key_file
                && identity.is_relative()
            {
                *identity = directory.join(&*identity);
            }
        }
        if let Some(runtime) = &mut configuration.runtime {
            runtime.resolve_paths(directory);
            if fs::canonicalize(&runtime.membership_file)?
                != fs::canonicalize(&configuration.membership_file)?
                || fs::canonicalize(&runtime.trust_root_file)?
                    != fs::canonicalize(&configuration.trust_root_file)?
            {
                bail!("runtime and local API must use the same membership store and trust root");
            }
        }
        Ok(configuration)
    }
}

pub fn read_secret(path: &Path) -> Result<String> {
    let encoded =
        avian_storage::read_secret_file(path, 64 * 1024).context("read protected credential")?;
    let secret = std::str::from_utf8(&encoded).context("credential is not UTF-8")?;
    let secret = secret.trim();
    if secret.len() < 32 || secret.chars().any(char::is_control) {
        bail!("credential must contain at least 32 characters and no control characters");
    }
    Ok(secret.to_owned())
}

pub fn create_private_file(path: &Path, bytes: &[u8]) -> Result<()> {
    avian_storage::create_private_file(path, bytes).context("create protected private file")
}

pub fn create_secret_file(path: &Path, bytes: &[u8]) -> Result<()> {
    avian_storage::create_secret_file(path, bytes).context("create protected credential")
}
