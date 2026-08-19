use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::path::Path;

use anyhow::{bail, Context};
use base64::{engine::general_purpose::STANDARD, Engine};
use ed25519_dalek::{SigningKey, VerifyingKey};
use mesh_core::{EmergencyAck, EmergencyAction, EmergencyCommand, NodeId};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::config::{CommandConfig, CommandMode};

const STATE_SCHEMA_VERSION: u16 = 1;
const MAX_KEY_FILE_BYTES: u64 = 4_096;

pub struct CommandRuntime {
    config: CommandConfig,
    node_id: NodeId,
    signing_key: Option<SigningKey>,
    issuers: BTreeMap<String, VerifyingKey>,
    state: PersistentState,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandEvaluation {
    AlreadyProcessed,
    Rejected(EmergencyAck),
    Accepted,
}

pub struct AckOutcome {
    pub verified: bool,
    pub accepted: bool,
    pub executed: bool,
    pub mavlink_result: Option<String>,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PersistentState {
    schema_version: u16,
    local_issuer_nonce: u64,
    processed_ids: BTreeSet<Uuid>,
    highest_nonce_by_issuer: BTreeMap<String, u64>,
    #[serde(default)]
    pending_execution: BTreeSet<Uuid>,
    #[serde(default)]
    pending_acks: BTreeMap<Uuid, EmergencyAck>,
}

impl Default for PersistentState {
    fn default() -> Self {
        Self {
            schema_version: STATE_SCHEMA_VERSION,
            local_issuer_nonce: 0,
            processed_ids: BTreeSet::new(),
            highest_nonce_by_issuer: BTreeMap::new(),
            pending_execution: BTreeSet::new(),
            pending_acks: BTreeMap::new(),
        }
    }
}

impl CommandRuntime {
    pub fn load(config: CommandConfig, node_id: NodeId) -> anyhow::Result<Self> {
        let signing_key = config
            .signing_key_file
            .as_deref()
            .map(read_signing_key)
            .transpose()?;
        let mut issuers = BTreeMap::new();
        for issuer in &config.issuers {
            if issuer.id.trim().is_empty() || issuers.contains_key(&issuer.id) {
                bail!("command issuer IDs must be nonempty and unique");
            }
            issuers.insert(
                issuer.id.clone(),
                read_verifying_key(&issuer.public_key_file)?,
            );
        }
        let state = read_state(&config.state_file)?;
        Ok(Self {
            config,
            node_id,
            signing_key,
            issuers,
            state,
        })
    }

    pub fn mode(&self) -> CommandMode {
        self.config.mode
    }

    pub fn poll_ms(&self) -> u64 {
        self.config.poll_ms
    }

    pub fn ack_timeout_ms(&self) -> u64 {
        self.config.ack_timeout_ms
    }

    pub fn retries(&self) -> u8 {
        self.config.retries
    }

    pub fn issue_rtl(&mut self, target: NodeId, now_ms: u64) -> anyhow::Result<EmergencyCommand> {
        if self.config.mode == CommandMode::Disabled {
            bail!("command mode is disabled");
        }
        let signing_key = self
            .signing_key
            .as_ref()
            .context("this node has no configured command signing key")?;
        self.state.local_issuer_nonce = self
            .state
            .local_issuer_nonce
            .checked_add(1)
            .context("local issuer nonce exhausted")?;
        persist_state(&self.config.state_file, &self.state)?;
        Ok(EmergencyCommand::issue(
            signing_key,
            Uuid::new_v4(),
            self.node_id.clone(),
            target,
            now_ms,
            now_ms.saturating_add(self.config.lifetime_ms),
            self.state.local_issuer_nonce,
            EmergencyAction::ReturnToLaunch,
        )?)
    }

    pub fn evaluate(
        &mut self,
        command: &EmergencyCommand,
        now_ms: u64,
        mavlink_system_locked: bool,
    ) -> anyhow::Result<CommandEvaluation> {
        if self.config.mode == CommandMode::Disabled || command.target != self.node_id {
            return Ok(CommandEvaluation::AlreadyProcessed);
        }
        if self.state.processed_ids.contains(&command.command_id) {
            return Ok(CommandEvaluation::AlreadyProcessed);
        }

        let mut verified = false;
        let rejection = if command.action != EmergencyAction::ReturnToLaunch {
            Some("action is not allowlisted".to_owned())
        } else if let Some(key) = self.issuers.get(command.issuer.as_str()) {
            match command.verify(key, now_ms) {
                Ok(()) => {
                    verified = true;
                    if command.expires_at_ms.saturating_sub(command.issued_at_ms)
                        > self.config.lifetime_ms
                    {
                        Some("command lifetime exceeds the configured acceptance window".to_owned())
                    } else if self
                        .state
                        .highest_nonce_by_issuer
                        .get(command.issuer.as_str())
                        .is_some_and(|nonce| command.nonce <= *nonce)
                    {
                        Some("command nonce is stale".to_owned())
                    } else if !mavlink_system_locked {
                        Some("MAVLink system is not locked".to_owned())
                    } else {
                        None
                    }
                }
                Err(error) => Some(error.to_string()),
            }
        } else {
            Some("issuer is not allowlisted".to_owned())
        };

        self.state.processed_ids.insert(command.command_id);
        if rejection.is_none() {
            self.state
                .highest_nonce_by_issuer
                .insert(command.issuer.to_string(), command.nonce);
            self.state.pending_execution.insert(command.command_id);
        }
        persist_state(&self.config.state_file, &self.state)?;

        if let Some(detail) = rejection {
            let ack = self.ack(
                command,
                AckOutcome {
                    verified,
                    accepted: false,
                    executed: false,
                    mavlink_result: None,
                    detail,
                },
                now_ms,
            );
            self.queue_ack(ack.clone())?;
            return Ok(CommandEvaluation::Rejected(ack));
        }
        Ok(CommandEvaluation::Accepted)
    }

    pub fn recover_interrupted(
        &mut self,
        command: &EmergencyCommand,
        now_ms: u64,
    ) -> anyhow::Result<Option<EmergencyAck>> {
        if !self.state.pending_execution.contains(&command.command_id) {
            return Ok(None);
        }
        let ack = self.ack(
            command,
            AckOutcome {
                verified: true,
                accepted: true,
                executed: false,
                mavlink_result: None,
                detail:
                    "execution suppressed after restart because acceptance was already persisted"
                        .into(),
            },
            now_ms,
        );
        self.queue_ack(ack.clone())?;
        Ok(Some(ack))
    }

    pub fn queue_ack(&mut self, ack: EmergencyAck) -> anyhow::Result<()> {
        self.state.pending_execution.remove(&ack.command_id);
        self.state.pending_acks.insert(ack.command_id, ack);
        persist_state(&self.config.state_file, &self.state)
    }

    pub fn mark_ack_published(&mut self, command_id: Uuid) -> anyhow::Result<()> {
        self.state.pending_acks.remove(&command_id);
        persist_state(&self.config.state_file, &self.state)
    }

    pub fn pending_ack(&self, command_id: Uuid) -> Option<EmergencyAck> {
        self.state.pending_acks.get(&command_id).cloned()
    }

    pub fn ack(
        &self,
        command: &EmergencyCommand,
        outcome: AckOutcome,
        now_ms: u64,
    ) -> EmergencyAck {
        EmergencyAck {
            command_id: command.command_id,
            node_id: self.node_id.clone(),
            accepted: outcome.accepted,
            verified: outcome.verified,
            executed: outcome.executed,
            command_mode: Some(mode_name(self.config.mode).into()),
            mavlink_result: outcome.mavlink_result,
            detail: outcome.detail,
            timestamp_ms: now_ms,
        }
    }
}

fn mode_name(mode: CommandMode) -> &'static str {
    match mode {
        CommandMode::Disabled => "disabled",
        CommandMode::DryRun => "dry_run",
        CommandMode::Execute => "execute",
    }
}

fn read_signing_key(path: &Path) -> anyhow::Result<SigningKey> {
    validate_private_permissions(path)?;
    let bytes = read_key_bytes(path)?;
    Ok(SigningKey::from_bytes(&bytes))
}

fn read_verifying_key(path: &Path) -> anyhow::Result<VerifyingKey> {
    VerifyingKey::from_bytes(&read_key_bytes(path)?)
        .with_context(|| format!("decoding Ed25519 public key {}", path.display()))
}

fn read_key_bytes(path: &Path) -> anyhow::Result<[u8; 32]> {
    let metadata = std::fs::metadata(path)
        .with_context(|| format!("reading key metadata {}", path.display()))?;
    anyhow::ensure!(
        metadata.is_file(),
        "key {} must be a regular file",
        path.display()
    );
    anyhow::ensure!(
        metadata.len() <= MAX_KEY_FILE_BYTES,
        "key {} exceeds {MAX_KEY_FILE_BYTES} bytes",
        path.display()
    );
    let encoded =
        std::fs::read_to_string(path).with_context(|| format!("reading key {}", path.display()))?;
    let decoded = STANDARD
        .decode(encoded.trim())
        .with_context(|| format!("decoding base64 key {}", path.display()))?;
    decoded
        .try_into()
        .map_err(|_| anyhow::anyhow!("key {} must decode to 32 bytes", path.display()))
}

#[cfg(unix)]
fn validate_private_permissions(path: &Path) -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let metadata = std::fs::metadata(path)
        .with_context(|| format!("reading permissions for {}", path.display()))?;
    anyhow::ensure!(
        metadata.is_file(),
        "private key {} must be a regular file",
        path.display()
    );
    let mode = metadata.permissions().mode();
    anyhow::ensure!(
        mode & 0o077 == 0,
        "private key {} must not be accessible by group or other users",
        path.display()
    );
    Ok(())
}

#[cfg(not(unix))]
fn validate_private_permissions(_path: &Path) -> anyhow::Result<()> {
    Ok(())
}

fn read_state(path: &Path) -> anyhow::Result<PersistentState> {
    match std::fs::read(path) {
        Ok(encoded) => {
            let state: PersistentState = serde_json::from_slice(&encoded)
                .with_context(|| format!("decoding command state {}", path.display()))?;
            anyhow::ensure!(
                state.schema_version == STATE_SCHEMA_VERSION,
                "unsupported command state schema {}",
                state.schema_version
            );
            Ok(state)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Ok(PersistentState::default())
        }
        Err(error) => {
            Err(error).with_context(|| format!("reading command state {}", path.display()))
        }
    }
}

fn persist_state(path: &Path, state: &PersistentState) -> anyhow::Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent)
        .with_context(|| format!("creating command state directory {}", parent.display()))?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent)
        .with_context(|| format!("creating temporary state in {}", parent.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        temporary
            .as_file()
            .set_permissions(std::fs::Permissions::from_mode(0o600))?;
    }
    temporary.write_all(&serde_json::to_vec(state)?)?;
    temporary.as_file_mut().sync_all()?;
    temporary
        .persist(path)
        .map_err(|error| error.error)
        .with_context(|| format!("atomically replacing command state {}", path.display()))?;
    sync_parent(parent)?;
    Ok(())
}

fn sync_parent(path: &Path) -> anyhow::Result<()> {
    std::fs::File::open(path)?.sync_all()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(directory: &Path, mode: CommandMode) -> (CommandConfig, SigningKey) {
        use std::os::unix::fs::PermissionsExt;
        let signing = SigningKey::from_bytes(&[7; 32]);
        let private = directory.join("ground.key");
        let public = directory.join("ground.pub");
        std::fs::write(
            &private,
            format!("{}\n", STANDARD.encode(signing.to_bytes())),
        )
        .unwrap();
        std::fs::set_permissions(&private, std::fs::Permissions::from_mode(0o600)).unwrap();
        std::fs::write(
            &public,
            format!("{}\n", STANDARD.encode(signing.verifying_key().to_bytes())),
        )
        .unwrap();
        (
            CommandConfig {
                mode,
                environment: crate::config::CommandEnvironment::Hardware,
                signing_key_file: Some(private),
                issuers: vec![crate::config::IssuerConfig {
                    id: "ground".into(),
                    public_key_file: public,
                }],
                state_file: directory.join("state.json"),
                lifetime_ms: 5_000,
                poll_ms: 250,
                ack_timeout_ms: 500,
                retries: 1,
            },
            signing,
        )
    }

    #[test]
    fn accepted_command_is_replay_safe_across_restart() {
        let directory = tempfile::tempdir().unwrap();
        let (config, signing) = config(directory.path(), CommandMode::DryRun);
        let command = EmergencyCommand::issue(
            &signing,
            Uuid::new_v4(),
            NodeId::from("ground"),
            NodeId::from("air"),
            1_000,
            6_000,
            1,
            EmergencyAction::ReturnToLaunch,
        )
        .unwrap();
        let mut runtime = CommandRuntime::load(config.clone(), NodeId::from("air")).unwrap();
        assert_eq!(
            runtime.evaluate(&command, 2_000, true).unwrap(),
            CommandEvaluation::Accepted
        );
        let mut restarted = CommandRuntime::load(config, NodeId::from("air")).unwrap();
        assert_eq!(
            restarted.evaluate(&command, 2_100, true).unwrap(),
            CommandEvaluation::AlreadyProcessed
        );
    }

    #[test]
    fn missing_system_lock_is_persistently_rejected() {
        let directory = tempfile::tempdir().unwrap();
        let (config, signing) = config(directory.path(), CommandMode::DryRun);
        let command = EmergencyCommand::issue(
            &signing,
            Uuid::new_v4(),
            NodeId::from("ground"),
            NodeId::from("air"),
            1_000,
            6_000,
            1,
            EmergencyAction::ReturnToLaunch,
        )
        .unwrap();
        let mut runtime = CommandRuntime::load(config, NodeId::from("air")).unwrap();
        let CommandEvaluation::Rejected(ack) = runtime.evaluate(&command, 2_000, false).unwrap()
        else {
            panic!("expected rejection");
        };
        assert!(ack.verified);
        assert!(!ack.accepted);
        assert!(!ack.executed);
        assert!(ack.detail.contains("not locked"));
    }

    #[test]
    fn signed_command_cannot_extend_the_acceptance_window() {
        let directory = tempfile::tempdir().unwrap();
        let (config, signing) = config(directory.path(), CommandMode::DryRun);
        let command = EmergencyCommand::issue(
            &signing,
            Uuid::new_v4(),
            NodeId::from("ground"),
            NodeId::from("air"),
            1_000,
            6_001,
            1,
            EmergencyAction::ReturnToLaunch,
        )
        .unwrap();
        let mut runtime = CommandRuntime::load(config, NodeId::from("air")).unwrap();
        let CommandEvaluation::Rejected(ack) = runtime.evaluate(&command, 2_000, true).unwrap()
        else {
            panic!("expected rejection");
        };
        assert!(ack.verified);
        assert!(!ack.accepted);
        assert!(!ack.executed);
        assert!(ack.detail.contains("acceptance window"));
    }
}
