use std::collections::{BTreeMap, BTreeSet};

use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

use crate::NodeId;

const MAX_CLOCK_SKEW_MS: u64 = 5_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EmergencyAction {
    GpsRescue,
    ReturnToLaunch,
    Land,
    Disarm,
}

impl EmergencyAction {
    fn wire_code(self) -> u8 {
        match self {
            Self::GpsRescue => 1,
            Self::ReturnToLaunch => 2,
            Self::Land => 3,
            Self::Disarm => 4,
        }
    }
}

/// A signed application command. Transport encryption is independent of this
/// signature so the command remains verifiable after store-and-forward.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmergencyCommand {
    pub command_id: Uuid,
    pub issuer: NodeId,
    pub target: NodeId,
    pub issued_at_ms: u64,
    pub expires_at_ms: u64,
    pub nonce: u64,
    pub action: EmergencyAction,
    pub signature: Vec<u8>,
}

impl EmergencyCommand {
    #[allow(clippy::too_many_arguments)]
    pub fn issue(
        signing_key: &SigningKey,
        command_id: Uuid,
        issuer: NodeId,
        target: NodeId,
        issued_at_ms: u64,
        expires_at_ms: u64,
        nonce: u64,
        action: EmergencyAction,
    ) -> Result<Self, CommandError> {
        if expires_at_ms <= issued_at_ms {
            return Err(CommandError::InvalidLifetime);
        }

        let mut command = Self {
            command_id,
            issuer,
            target,
            issued_at_ms,
            expires_at_ms,
            nonce,
            action,
            signature: Vec::new(),
        };
        command.signature = signing_key
            .sign(&command.signing_bytes())
            .to_bytes()
            .to_vec();
        Ok(command)
    }

    pub fn verify(&self, verifying_key: &VerifyingKey, now_ms: u64) -> Result<(), CommandError> {
        if self.expires_at_ms <= self.issued_at_ms {
            return Err(CommandError::InvalidLifetime);
        }
        if now_ms > self.expires_at_ms {
            return Err(CommandError::Expired);
        }
        if self.issued_at_ms > now_ms.saturating_add(MAX_CLOCK_SKEW_MS) {
            return Err(CommandError::IssuedInFuture);
        }

        let signature = Signature::try_from(self.signature.as_slice())
            .map_err(|_| CommandError::InvalidSignature)?;
        verifying_key
            .verify(&self.signing_bytes(), &signature)
            .map_err(|_| CommandError::InvalidSignature)
    }

    fn signing_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(96);
        bytes.extend_from_slice(b"avian/emergency-command/v1\0");
        bytes.extend_from_slice(self.command_id.as_bytes());
        append_string(&mut bytes, self.issuer.as_str());
        append_string(&mut bytes, self.target.as_str());
        bytes.extend_from_slice(&self.issued_at_ms.to_be_bytes());
        bytes.extend_from_slice(&self.expires_at_ms.to_be_bytes());
        bytes.extend_from_slice(&self.nonce.to_be_bytes());
        bytes.push(self.action.wire_code());
        bytes
    }
}

fn append_string(bytes: &mut Vec<u8>, value: &str) {
    let value_bytes = value.as_bytes();
    let length = u32::try_from(value_bytes.len()).expect("node identifier must fit in u32");
    bytes.extend_from_slice(&length.to_be_bytes());
    bytes.extend_from_slice(value_bytes);
}

/// Receiver-local replay protection. Production persistence will store this
/// state across mesh-agent restarts.
#[derive(Debug, Default)]
pub struct ReplayGuard {
    accepted_ids: BTreeSet<Uuid>,
    highest_nonce_by_issuer: BTreeMap<NodeId, u64>,
}

impl ReplayGuard {
    pub fn accept(
        &mut self,
        command: &EmergencyCommand,
        expected_target: &NodeId,
        verifying_key: &VerifyingKey,
        now_ms: u64,
    ) -> Result<(), CommandError> {
        command.verify(verifying_key, now_ms)?;
        if &command.target != expected_target {
            return Err(CommandError::WrongTarget);
        }
        if self.accepted_ids.contains(&command.command_id) {
            return Err(CommandError::Replay);
        }
        if self
            .highest_nonce_by_issuer
            .get(&command.issuer)
            .is_some_and(|highest| command.nonce <= *highest)
        {
            return Err(CommandError::Replay);
        }

        self.accepted_ids.insert(command.command_id);
        self.highest_nonce_by_issuer
            .insert(command.issuer.clone(), command.nonce);
        Ok(())
    }
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum CommandError {
    #[error("command expiry must be after issuance")]
    InvalidLifetime,
    #[error("command has expired")]
    Expired,
    #[error("command issuance time is too far in the future")]
    IssuedInFuture,
    #[error("command signature is invalid")]
    InvalidSignature,
    #[error("command targets another node")]
    WrongTarget,
    #[error("command has already been accepted or has a stale nonce")]
    Replay,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn command(key: &SigningKey) -> EmergencyCommand {
        EmergencyCommand::issue(
            key,
            Uuid::from_u128(1),
            NodeId::from("ground"),
            NodeId::from("beta"),
            1_000,
            6_000,
            1,
            EmergencyAction::GpsRescue,
        )
        .unwrap()
    }

    #[test]
    fn verifies_and_rejects_replay() {
        let key = SigningKey::from_bytes(&[7_u8; 32]);
        let verifying_key = key.verifying_key();
        let command = command(&key);
        let mut guard = ReplayGuard::default();

        guard
            .accept(&command, &NodeId::from("beta"), &verifying_key, 2_000)
            .unwrap();
        assert_eq!(
            guard.accept(&command, &NodeId::from("beta"), &verifying_key, 2_000),
            Err(CommandError::Replay)
        );
    }

    #[test]
    fn tampering_invalidates_signature() {
        let key = SigningKey::from_bytes(&[7_u8; 32]);
        let mut command = command(&key);
        command.action = EmergencyAction::Disarm;

        assert_eq!(
            command.verify(&key.verifying_key(), 2_000),
            Err(CommandError::InvalidSignature)
        );
    }
}
