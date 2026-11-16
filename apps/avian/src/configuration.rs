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
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ChudConfiguration {
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
        Ok(configuration)
    }
}

pub fn read_secret(path: &Path) -> Result<String> {
    let metadata = fs::symlink_metadata(path).context("inspect credential file")?;
    if !metadata.file_type().is_file() {
        bail!("credential path must be a regular file, not a symbolic link");
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o077 != 0 {
            bail!("credential file must be accessible only by its owner");
        }
    }
    let secret = fs::read_to_string(path).context("read credential file")?;
    let secret = secret.trim();
    if secret.len() < 32 || secret.chars().any(char::is_control) {
        bail!("credential must contain at least 32 characters and no control characters");
    }
    Ok(secret.to_owned())
}

pub fn create_private_file(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write;
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(path)
        .context("create private file without overwriting")?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}
