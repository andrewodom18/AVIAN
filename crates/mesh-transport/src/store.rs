//! Transactional, bounded whole-record registers. All peers use the same total
//! version order, so concurrent records converge without arrival-order ties.
use std::path::Path;

use anyhow::{bail, Context};
use mesh_core::DeliveryClass;
use redb::{Database, ReadableTable, TableDefinition};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::AvianRecord;

pub(crate) const MAX_RECORD_BYTES: usize = 1024 * 1024;
const MAX_STORE_BYTES: usize = 64 * 1024 * 1024;
const MAX_RECORDS: usize = 65_536;
pub(crate) const PAGE_SIZE: usize = 64;
const RECORDS: TableDefinition<&str, &[u8]> = TableDefinition::new("avian_records_v1");
const IDENTITY: TableDefinition<&str, &str> = TableDefinition::new("avian_identity_v1");

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub(crate) struct Version {
    published_at_ms: u64,
    sequence: u64,
    source: String,
    digest: [u8; 32],
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct InventoryEntry {
    pub key: String,
    pub version: Version,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct StoredRecord {
    pub record_id: String,
    pub record: AvianRecord,
}

impl StoredRecord {
    fn encoded(&self) -> anyhow::Result<Vec<u8>> {
        // Canonical object-key order also covers any maps inside domain payloads.
        let encoded = serde_json::to_vec(&serde_json::to_value(self)?)?;
        let decoded: StoredRecord = serde_json::from_slice(&encoded)?;
        if decoded.record_id != self.record_id || decoded.record != self.record {
            bail!("AVIAN record cannot round-trip JSON without changing its value");
        }
        Ok(encoded)
    }
    fn version(&self) -> anyhow::Result<Version> {
        Ok(Version {
            published_at_ms: self.record.published_at_ms,
            sequence: self.record.sequence,
            source: self.record.source.as_str().to_owned(),
            digest: Sha256::digest(self.encoded()?).into(),
        })
    }
    fn key(&self) -> String {
        record_key(self.record.class, &self.record_id)
    }
}

pub(crate) fn record_key(class: DeliveryClass, id: &str) -> String {
    let prefix = match class {
        DeliveryClass::Emergency => '0',
        DeliveryClass::Acknowledgement => '1',
        DeliveryClass::Mission => '2',
        DeliveryClass::Telemetry => '3',
        DeliveryClass::Bulk => '4',
    };
    format!("{prefix}:{id}")
}

pub(crate) struct Store {
    db: Database,
    bytes: usize,
    count: usize,
}

impl Store {
    pub fn open(path: &Path, formation: &str, endpoint: &str) -> anyhow::Result<Self> {
        std::fs::create_dir_all(path)?;
        let database_path = path.join("avian-state-v1.redb");
        if !database_path.exists() {
            for entry in std::fs::read_dir(path)? {
                let name = entry?.file_name();
                // Application-owned pairing/journal files may precede transport startup.
                if !matches!(
                    name.to_str(),
                    Some("paired-peers.json" | "command-state.json" | "command-journal.json")
                ) {
                    bail!("unrecognized existing transport state; use a new empty storage directory and reprovision peers; automatic state migration is unsupported");
                }
            }
        }
        let db = Database::create(database_path).context("open AVIAN transactional storage")?;
        let write = db.begin_write()?;
        {
            write.open_table(RECORDS)?;
            let mut identity = write.open_table(IDENTITY)?;
            for (key, expected) in [("formation", formation), ("endpoint", endpoint)] {
                if let Some(existing) = identity.get(key)? {
                    if existing.value() != expected {
                        bail!("persistent storage belongs to another formation or node identity");
                    }
                }
                identity.insert(key, expected)?;
            }
        }
        write.commit()?;
        let read = db.begin_read()?;
        let table = read.open_table(RECORDS)?;
        let mut count = 0;
        let mut bytes = 0;
        for row in table.iter()? {
            let (key, encoded) = row?;
            let stored: StoredRecord = serde_json::from_slice(encoded.value())?;
            crate::node::validate_record_id(&stored.record_id)?;
            stored.record.validate()?;
            if key.value() != stored.key() || encoded.value().len() > MAX_RECORD_BYTES {
                bail!("invalid persisted AVIAN record");
            }
            count += 1;
            bytes += encoded.value().len();
        }
        drop(table);
        drop(read);
        if count > MAX_RECORDS || bytes > MAX_STORE_BYTES {
            bail!("AVIAN storage exceeds its record or byte limit");
        }
        Ok(Self { db, bytes, count })
    }

    pub fn get(&self, key: &str) -> anyhow::Result<Option<StoredRecord>> {
        let read = self.db.begin_read()?;
        let table = read.open_table(RECORDS)?;
        let encoded = table.get(key)?;
        encoded
            .map(|value| serde_json::from_slice(value.value()).map_err(Into::into))
            .transpose()
    }

    pub fn merge(&mut self, stored: StoredRecord) -> anyhow::Result<bool> {
        crate::node::validate_record_id(&stored.record_id)?;
        stored.record.validate()?;
        let encoded = stored.encoded()?;
        if encoded.len() > MAX_RECORD_BYTES {
            bail!("AVIAN record exceeds 1 MiB limit");
        }
        let key = stored.key();
        let previous = self.get(&key)?;
        if let Some(previous) = &previous {
            if previous.version()? >= stored.version()? {
                return Ok(false);
            }
        }
        let previous_bytes = previous
            .as_ref()
            .map(|value| value.encoded().map(|bytes| bytes.len()))
            .transpose()?
            .unwrap_or(0);
        let next_bytes = self.bytes - previous_bytes + encoded.len();
        let next_count = self.count + usize::from(previous.is_none());
        if next_bytes > MAX_STORE_BYTES || next_count > MAX_RECORDS {
            bail!(
                "AVIAN storage is full (64 MiB or 65536 records); operator retention is required"
            );
        }
        let write = self.db.begin_write()?;
        {
            write
                .open_table(RECORDS)?
                .insert(key.as_str(), encoded.as_slice())?;
        }
        write.commit()?;
        self.bytes = next_bytes;
        self.count = next_count;
        Ok(true)
    }

    pub fn inventory(&self, after: Option<&str>) -> anyhow::Result<(Vec<InventoryEntry>, bool)> {
        let read = self.db.begin_read()?;
        let table = read.open_table(RECORDS)?;
        let mut entries = Vec::with_capacity(PAGE_SIZE);
        let mut more = false;
        for row in table.range(after.unwrap_or("")..)? {
            let (key, value) = row?;
            if after == Some(key.value()) {
                continue;
            }
            if entries.len() == PAGE_SIZE {
                more = true;
                break;
            }
            let stored: StoredRecord = serde_json::from_slice(value.value())?;
            entries.push(InventoryEntry {
                key: key.value().to_owned(),
                version: stored.version()?,
            });
        }
        Ok((entries, more))
    }

    pub fn inventory_keys(&self, keys: &[String]) -> anyhow::Result<Vec<InventoryEntry>> {
        let mut result = Vec::new();
        for key in keys {
            if let Some(record) = self.get(key)? {
                result.push(InventoryEntry {
                    key: key.clone(),
                    version: record.version()?,
                });
            }
        }
        result.sort_by(|left, right| left.key.cmp(&right.key));
        Ok(result)
    }

    pub fn needed(&self, entries: &[InventoryEntry]) -> anyhow::Result<Vec<String>> {
        let mut result = Vec::new();
        for entry in entries {
            if entry.key.len() > 258 {
                bail!("invalid inventory key");
            }
            if self
                .get(&entry.key)?
                .map(|record| record.version())
                .transpose()?
                .is_none_or(|local| local < entry.version)
            {
                result.push(entry.key.clone());
            }
        }
        Ok(result)
    }

    pub fn scan(&self, class: DeliveryClass) -> anyhow::Result<Vec<(String, AvianRecord)>> {
        let read = self.db.begin_read()?;
        let table = read.open_table(RECORDS)?;
        let prefix = record_key(class, "");
        let mut result = Vec::new();
        for row in table.range(prefix.as_str()..)? {
            let (key, value) = row?;
            if !key.value().starts_with(&prefix) {
                break;
            }
            let stored: StoredRecord = serde_json::from_slice(value.value())?;
            result.push((stored.record_id, stored.record));
        }
        Ok(result)
    }
}
