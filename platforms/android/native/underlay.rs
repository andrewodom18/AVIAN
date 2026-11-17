//! Reconcile physical interfaces through the embedded engine's private in-process API.
use anyhow::{Context, Result, bail};
use easytier::proto::{
    api::config::{ConfigPatchAction, InstanceConfigPatch, PatchConfigRequest, UrlPatch},
    rpc_types::controller::BaseController,
};
use serde::Deserialize;
use std::{
    collections::{BTreeMap, BTreeSet},
    net::IpAddr,
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Assignment {
    pub address: IpAddr,
    pub interface: String,
    pub network_id: u64,
}

#[derive(Default)]
pub struct Underlays {
    connectors: BTreeSet<String>,
    listeners: BTreeSet<String>,
    networks: BTreeMap<(IpAddr, String), u64>,
}

impl Underlays {
    pub async fn reconcile(
        &mut self,
        peers: &[String],
        assignments: &[Assignment],
    ) -> Result<bool> {
        if assignments.len() > 128 {
            bail!("too many physical addresses");
        }
        let mut observed = BTreeMap::new();
        let mut ambiguous = BTreeSet::new();
        for assignment in assignments {
            let key = (assignment.address, assignment.interface.clone());
            if observed
                .insert(key.clone(), assignment.network_id)
                .is_some_and(|previous| previous != assignment.network_id)
            {
                ambiguous.insert(key);
            }
        }
        for key in &ambiguous {
            observed.remove(key);
        }
        let mut connectors = BTreeSet::new();
        let mut listeners = BTreeSet::new();
        for assignment in assignments {
            if ambiguous.contains(&(assignment.address, assignment.interface.clone())) {
                continue;
            }
            if assignment.network_id == 0
                || assignment.interface.is_empty()
                || assignment.interface.len() > 64
                || assignment
                    .interface
                    .chars()
                    .any(|character| character.is_control())
                || assignment.address.is_unspecified()
                || assignment.address.is_loopback()
                || assignment.address.is_multicast()
            {
                bail!("invalid physical assignment");
            }
            for protocol in ["tcp", "udp"] {
                let mut listener = url::Url::parse(&format!(
                    "{protocol}://{}:11010",
                    socket_host(assignment.address)
                ))?;
                bind_query(&mut listener, assignment);
                listeners.insert(listener.to_string());
            }
            for configured in peers {
                let mut peer: url::Url = configured.parse()?;
                let remote: IpAddr = peer
                    .host_str()
                    .context("missing peer address")?
                    .trim_matches(['[', ']'])
                    .parse()?;
                if remote.is_ipv4() != assignment.address.is_ipv4() {
                    continue;
                }
                if peer.query().is_some() {
                    let options: std::collections::BTreeMap<_, _> =
                        peer.query_pairs().into_owned().collect();
                    if options.len() != 2
                        || options.get("avian_bind") != Some(&assignment.address.to_string())
                        || options.get("avian_interface") != Some(&assignment.interface)
                    {
                        continue;
                    }
                }
                bind_query(&mut peer, assignment);
                connectors.insert(peer.to_string());
            }
        }
        let service = super::instance_service().await?;
        let mut complete = ambiguous.is_empty();
        for (key, identifier) in &observed {
            if self
                .networks
                .get(key)
                .is_some_and(|previous| previous != identifier)
            {
                // A replacement Network can reuse an interface and address. The
                // old Android network mark cannot be reused for that listener.
                let mut released = true;
                for (current, listener) in
                    [(&mut self.listeners, true), (&mut self.connectors, false)]
                {
                    for address in current
                        .iter()
                        .filter(|address| binding_key(address).as_ref() == Some(key))
                        .cloned()
                        .collect::<Vec<_>>()
                    {
                        if patch(
                            service.as_ref(),
                            &address,
                            ConfigPatchAction::Remove,
                            listener,
                        )
                        .await
                        .is_ok()
                        {
                            current.remove(&address);
                        } else {
                            released = false;
                        }
                    }
                }
                if released {
                    self.networks.insert(key.clone(), *identifier);
                } else {
                    complete = false;
                }
            } else {
                self.networks.insert(key.clone(), *identifier);
            }
        }
        self.networks.retain(|key, _| observed.contains_key(key));
        // Establish replacements first. Stable sockets are retained across capability callbacks.
        for (desired, current, listener) in [
            (&listeners, &mut self.listeners, true),
            (&connectors, &mut self.connectors, false),
        ] {
            for address in desired.difference(current).cloned().collect::<Vec<_>>() {
                if patch(service.as_ref(), &address, ConfigPatchAction::Add, listener)
                    .await
                    .is_ok()
                {
                    current.insert(address);
                } else {
                    // A physical address can disappear between callback and bind.
                    // Retain independent working paths and retry on the next update.
                    complete = false;
                }
            }
            for address in current.difference(desired).cloned().collect::<Vec<_>>() {
                if patch(
                    service.as_ref(),
                    &address,
                    ConfigPatchAction::Remove,
                    listener,
                )
                .await
                .is_ok()
                {
                    current.remove(&address);
                } else {
                    complete = false;
                }
            }
        }
        Ok(complete)
    }
}

fn socket_host(address: IpAddr) -> String {
    match address {
        IpAddr::V4(_) => address.to_string(),
        IpAddr::V6(_) => format!("[{address}]"),
    }
}
fn bind_query(address: &mut url::Url, assignment: &Assignment) {
    address.set_query(None);
    address
        .query_pairs_mut()
        .append_pair("avian_bind", &assignment.address.to_string())
        .append_pair("avian_interface", &assignment.interface);
}

async fn patch(
    service: &dyn easytier::rpc_service::InstanceRpcService,
    address: &str,
    action: ConfigPatchAction,
    listener: bool,
) -> Result<()> {
    let change = UrlPatch {
        action: action.into(),
        url: Some(address.parse::<url::Url>()?.into()),
    };
    let mut changes = InstanceConfigPatch::default();
    if listener {
        changes.local_listeners.push(change);
    } else {
        changes.connectors.push(change);
    }
    service
        .get_config_service()
        .patch_config(
            BaseController::default(),
            PatchConfigRequest {
                instance: None,
                patch: Some(changes),
            },
        )
        .await?;
    Ok(())
}

fn binding_key(address: &str) -> Option<(IpAddr, String)> {
    let address = url::Url::parse(address).ok()?;
    let options: BTreeMap<_, _> = address.query_pairs().into_owned().collect();
    Some((
        options.get("avian_bind")?.parse().ok()?,
        options.get("avian_interface")?.clone(),
    ))
}
