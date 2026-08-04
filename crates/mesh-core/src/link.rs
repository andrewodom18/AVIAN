use serde::{Deserialize, Serialize};

use crate::{DeliveryClass, DeliveryPolicy};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct LinkId(pub String);

impl From<&str> for LinkId {
    fn from(value: &str) -> Self {
        Self(value.to_owned())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransportKind {
    Ethernet,
    Wifi,
    Cellular,
    SubGhz,
    Satellite,
    Bluetooth,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct LinkMetrics {
    pub latency_ms: f32,
    pub loss_ratio: f32,
    pub goodput_bps: f32,
    pub signal_quality: f32,
    pub stability: f32,
    pub energy_cost: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct LinkGeometry {
    pub distance_m: f64,
    pub line_of_sight: bool,
    /// A value of 1.0 means the estimated first Fresnel zone is clear.
    pub fresnel_clearance_ratio: f32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LinkCandidate {
    pub id: LinkId,
    pub transport: TransportKind,
    pub available: bool,
    pub metrics: LinkMetrics,
    pub geometry: LinkGeometry,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RoutePlan {
    pub primary: LinkId,
    pub redundant: Vec<LinkId>,
    pub ranked_scores: Vec<(LinkId, f32)>,
}

/// Application-level selector layered above PEAT's transport manager. It adds
/// RF geometry and hysteresis that are specific to moving aircraft.
#[derive(Debug, Clone, Copy)]
pub struct LinkOrchestrator {
    /// Minimum score improvement required before replacing a healthy primary.
    pub switch_margin: f32,
}

impl Default for LinkOrchestrator {
    fn default() -> Self {
        Self { switch_margin: 8.0 }
    }
}

impl LinkOrchestrator {
    pub fn select(
        &self,
        candidates: &[LinkCandidate],
        class: DeliveryClass,
        current_primary: Option<&LinkId>,
    ) -> Option<RoutePlan> {
        let mut ranked: Vec<(LinkId, f32)> = candidates
            .iter()
            .filter_map(|candidate| {
                score(candidate, class).map(|value| (candidate.id.clone(), value))
            })
            .collect();
        ranked.sort_by(|left, right| {
            right
                .1
                .total_cmp(&left.1)
                .then_with(|| left.0.cmp(&right.0))
        });
        if ranked.is_empty() {
            return None;
        }

        if let Some(current) = current_primary {
            if let Some(current_index) = ranked.iter().position(|(id, _)| id == current) {
                let improvement = ranked[0].1 - ranked[current_index].1;
                if improvement < self.switch_margin {
                    let retained = ranked.remove(current_index);
                    ranked.insert(0, retained);
                }
            }
        }

        let policy = DeliveryPolicy::for_class(class);
        let selected_count = usize::from(policy.redundant_paths).min(ranked.len());
        let primary = ranked[0].0.clone();
        let redundant = ranked
            .iter()
            .skip(1)
            .take(selected_count.saturating_sub(1))
            .map(|(id, _)| id.clone())
            .collect();

        Some(RoutePlan {
            primary,
            redundant,
            ranked_scores: ranked,
        })
    }
}

fn score(candidate: &LinkCandidate, class: DeliveryClass) -> Option<f32> {
    if !candidate.available || !metrics_valid(&candidate.metrics) {
        return None;
    }

    let metrics = candidate.metrics;
    let latency = 1.0 - (metrics.latency_ms / 2_000.0).clamp(0.0, 1.0);
    let reliability = 1.0 - metrics.loss_ratio;
    let goodput = (metrics.goodput_bps / 10_000_000.0).clamp(0.0, 1.0);
    let efficiency = 1.0 - metrics.energy_cost;
    let geometry = if candidate.geometry.line_of_sight {
        1.0
    } else {
        0.55
    } * candidate.geometry.fresnel_clearance_ratio.clamp(0.2, 1.0);

    let weighted = match class {
        DeliveryClass::Emergency | DeliveryClass::Acknowledgement => {
            0.30 * reliability
                + 0.25 * latency
                + 0.20 * metrics.stability
                + 0.15 * metrics.signal_quality
                + 0.10 * geometry
        }
        DeliveryClass::Mission => {
            0.30 * reliability
                + 0.20 * metrics.stability
                + 0.20 * goodput
                + 0.15 * metrics.signal_quality
                + 0.15 * geometry
        }
        DeliveryClass::Telemetry => {
            0.25 * latency
                + 0.25 * reliability
                + 0.20 * metrics.signal_quality
                + 0.15 * metrics.stability
                + 0.15 * geometry
        }
        DeliveryClass::Bulk => {
            0.40 * goodput
                + 0.20 * reliability
                + 0.15 * metrics.stability
                + 0.15 * efficiency
                + 0.10 * geometry
        }
    };
    Some(weighted * 100.0)
}

fn metrics_valid(metrics: &LinkMetrics) -> bool {
    metrics.latency_ms.is_finite()
        && metrics.latency_ms >= 0.0
        && metrics.goodput_bps.is_finite()
        && metrics.goodput_bps >= 0.0
        && [
            metrics.loss_ratio,
            metrics.signal_quality,
            metrics.stability,
            metrics.energy_cost,
        ]
        .into_iter()
        .all(|value| value.is_finite() && (0.0..=1.0).contains(&value))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate(id: &str, latency_ms: f32, loss_ratio: f32) -> LinkCandidate {
        LinkCandidate {
            id: LinkId::from(id),
            transport: TransportKind::Wifi,
            available: true,
            metrics: LinkMetrics {
                latency_ms,
                loss_ratio,
                goodput_bps: 2_000_000.0,
                signal_quality: 0.8,
                stability: 0.9,
                energy_cost: 0.3,
            },
            geometry: LinkGeometry {
                distance_m: 10_000.0,
                line_of_sight: true,
                fresnel_clearance_ratio: 1.0,
            },
        }
    }

    #[test]
    fn emergency_routes_use_two_available_paths() {
        let candidates = vec![
            candidate("wifi", 30.0, 0.01),
            candidate("cellular", 80.0, 0.03),
            candidate("sub-ghz", 500.0, 0.10),
        ];
        let plan = LinkOrchestrator::default()
            .select(&candidates, DeliveryClass::Emergency, None)
            .unwrap();

        assert_eq!(plan.primary, LinkId::from("wifi"));
        assert_eq!(plan.redundant.len(), 1);
        assert_eq!(plan.redundant[0], LinkId::from("cellular"));
    }

    #[test]
    fn unavailable_primary_is_replaced() {
        let mut wifi = candidate("wifi", 30.0, 0.01);
        wifi.available = false;
        let cellular = candidate("cellular", 80.0, 0.03);
        let plan = LinkOrchestrator::default()
            .select(
                &[wifi, cellular],
                DeliveryClass::Telemetry,
                Some(&LinkId::from("wifi")),
            )
            .unwrap();

        assert_eq!(plan.primary, LinkId::from("cellular"));
    }
}
