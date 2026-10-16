//! Serial control/command ownership, isolated from telemetry and status service.
use mesh_agent::commands::CommandRuntime;
use mesh_agent::config::{CommandMode, ResolvedConfig, TaggedPeer};
use mesh_agent::control::ControlEnvelope;
use mesh_agent::protocol::{ControlRequest, ControlResponse};
use mesh_agent::status::{AgentStatus, CommandStatus, MavlinkStatus, StatusError};
use mesh_core::NodeId;
use mesh_transport::{MeshNode, PeerDescriptor};
use tokio::sync::{mpsc, oneshot, watch};
use tokio::time::{self, Duration, MissedTickBehavior};
use vehicle_adapters::MavlinkCommandSender;

use crate::{peer_management::PeerRegistry, process_emergency_commands, unix_time_ms};

pub(super) struct RuntimeUpdate {
    // Only configuration intent crosses back to the reactor. Live connection
    // status remains owned there, so slow operations cannot overwrite it.
    pub(super) peer_registry: Option<(Vec<TaggedPeer>, Vec<PeerDescriptor>)>,
    pub(super) commands: CommandStatus,
    pub(super) errors: Vec<StatusError>,
    pub(super) response: Option<(oneshot::Sender<ControlResponse>, ControlResponse)>,
}

pub(super) struct ControlRuntime {
    pub(super) requests: mpsc::Sender<ControlEnvelope>,
    pub(super) updates: mpsc::Receiver<RuntimeUpdate>,
    mavlink: watch::Sender<MavlinkStatus>,
    stop: oneshot::Sender<()>,
    task: tokio::task::JoinHandle<anyhow::Result<()>>,
}

impl ControlRuntime {
    pub(super) fn start(
        node: MeshNode,
        config: ResolvedConfig,
        mut registry: PeerRegistry,
        mut commands: CommandRuntime,
        mut status: AgentStatus,
        mavlink_commands: Option<MavlinkCommandSender>,
    ) -> Self {
        let (requests, mut receiver) = mpsc::channel::<ControlEnvelope>(32);
        let (updates, update_receiver) = mpsc::channel::<RuntimeUpdate>(32);
        let (mavlink, mavlink_status) = watch::channel(status.mavlink.clone());
        let (stop, mut stopping) = oneshot::channel();
        // Command journals and paired-peer files deliberately use synchronous,
        // atomic fsync/rename transactions. One dedicated blocking worker owns
        // those writes; no filesystem latency or network wait pauses telemetry.
        // The worker is joined before the transport shuts down.
        // Keep transport futures on the process executor: creating and then
        // dropping a nested runtime could cancel tasks lazily spawned by a dial
        // before the shared endpoint has completed its own shutdown.
        let executor = tokio::runtime::Handle::current();
        let task = tokio::task::spawn_blocking(move || {
            executor.block_on(async move {
                let node_id = NodeId::from(config.name.clone());
                let mut command_poll = time::interval(Duration::from_millis(commands.poll_ms()));
                command_poll.set_missed_tick_behavior(MissedTickBehavior::Skip);
                let mut ack_sequence = 0;
                loop {
                    let peer_changed;
                    let response;
                    tokio::select! {
                        _ = &mut stopping => break,
                        envelope = receiver.recv() => {
                            let Some(envelope) = envelope else { break };
                            if envelope.respond_to.is_closed() { continue; }
                            peer_changed = matches!(&envelope.request, ControlRequest::ConfigurePeer { .. }
                                | ControlRequest::SetPeerPaths { .. } | ControlRequest::RemovePeer { .. });
                            status.mavlink = mavlink_status.borrow().clone();
                            let (respond_to, replied) = oneshot::channel();
                            registry.handle(&node, &mut status, &mut commands, &config,
                                ControlEnvelope { request: envelope.request, respond_to }).await;
                            response = replied.await.ok().map(|reply| (envelope.respond_to, reply));
                        }
                        _ = command_poll.tick(), if commands.mode() != CommandMode::Disabled => {
                            peer_changed = false;
                            response = None;
                            status.mavlink = mavlink_status.borrow().clone();
                            process_emergency_commands(&node, &node_id, &mut commands, &mut status,
                                mavlink_commands.as_ref(), &mut ack_sequence, &mavlink_status).await;
                        }
                    }
                    let update = RuntimeUpdate {
                        peer_registry: peer_changed.then(|| (registry.tagged.clone(), registry.peers.clone())),
                        commands: status.commands.clone(),
                        errors: std::mem::take(&mut status.last_errors),
                        response,
                    };
                    tokio::select! {
                        biased;
                        _ = &mut stopping => break,
                        sent = updates.send(update) => if sent.is_err() { break },
                    }
                }
                Ok(())
            })
        });
        Self {
            requests,
            updates: update_receiver,
            mavlink,
            stop,
            task,
        }
    }

    pub(super) fn update_mavlink(&self, status: MavlinkStatus) {
        self.mavlink.send_replace(status);
    }

    pub(super) async fn shutdown(self) -> anyhow::Result<()> {
        let _ = self.stop.send(());
        // In-flight command work completes its durable acknowledgement before
        // releasing the node. Queued requests are abandoned with their clients.
        self.task.await??;
        Ok(())
    }
}

/// Status never joins the work queue, including when the queue is saturated.
/// Mutating requests are bounded and remain serialized by their owner.
pub(super) fn dispatch_control(
    envelope: ControlEnvelope,
    status: &AgentStatus,
    worker: &mpsc::Sender<ControlEnvelope>,
) {
    if matches!(&envelope.request, ControlRequest::Status { .. }) {
        let _ = envelope.respond_to.send(ControlResponse::Status {
            status: Box::new(status.snapshot(unix_time_ms())),
        });
    } else if let Err(error) = worker.try_send(envelope) {
        let (code, detail) = match &error {
            mpsc::error::TrySendError::Full(_) => ("control_busy", "control work queue is full"),
            mpsc::error::TrySendError::Closed(_) => {
                ("control_unavailable", "control worker stopped")
            }
        };
        let _ = error.into_inner().respond_to.send(ControlResponse::Error {
            code: code.into(),
            detail: detail.into(),
        });
    }
}

pub(super) async fn shutdown_signal() -> std::io::Result<()> {
    #[cfg(unix)]
    {
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
        tokio::select! {
            result = tokio::signal::ctrl_c() => result,
            _ = terminate.recv() => Ok(()),
        }
    }
    #[cfg(not(unix))]
    tokio::signal::ctrl_c().await
}

#[cfg(test)]
mod tests {
    use super::*;
    use mesh_agent::config::ConfiguredNodeRole;

    fn status() -> AgentStatus {
        AgentStatus::new(
            "test".into(),
            ConfiguredNodeRole::Ground,
            0,
            CommandMode::Disabled,
            false,
            false,
            0,
        )
    }

    fn envelope(request: ControlRequest) -> (ControlEnvelope, oneshot::Receiver<ControlResponse>) {
        let (respond_to, response) = oneshot::channel();
        (
            ControlEnvelope {
                request,
                respond_to,
            },
            response,
        )
    }

    #[tokio::test]
    async fn status_remains_responsive_while_control_work_is_backlogged() {
        let (sender, _receiver) = mpsc::channel(1);
        let (queued, _response) = envelope(ControlRequest::ListRecords {
            class: mesh_core::DeliveryClass::Telemetry,
            limit: 1,
        });
        sender.send(queued).await.unwrap();
        let (request, response) = envelope(ControlRequest::Status {
            require_ready: false,
        });
        dispatch_control(request, &status(), &sender);
        assert!(matches!(
            time::timeout(Duration::from_millis(50), response)
                .await
                .unwrap()
                .unwrap(),
            ControlResponse::Status { .. }
        ));
        assert_eq!(sender.capacity(), 0, "status must bypass queued mutations");
    }

    #[tokio::test]
    async fn saturated_work_queue_rejects_mutation_without_waiting() {
        let (sender, _receiver) = mpsc::channel(1);
        let request = || ControlRequest::ListRecords {
            class: mesh_core::DeliveryClass::Telemetry,
            limit: 1,
        };
        let (first, _) = envelope(request());
        sender.send(first).await.unwrap();
        let (second, response) = envelope(request());
        dispatch_control(second, &status(), &sender);
        assert!(
            matches!(response.await.unwrap(), ControlResponse::Error { code, .. } if code == "control_busy")
        );
    }
}
