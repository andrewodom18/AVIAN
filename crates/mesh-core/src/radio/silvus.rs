//! Dry-run vendor command templates; this module never contacts hardware.

use super::*;

impl ArcRadioConfiguration {
    pub fn silvus_apply_templates(
        &self,
    ) -> Result<Vec<SilvusGroupApplyTemplate>, RadioConfigError> {
        let assessment = self.assess()?;
        self.fleet
            .groups
            .iter()
            .map(|group| {
                let mut steps = vec![SilvusApiStep {
                    effect: SilvusStepEffect::Verify,
                    method: Some("supported_frequency_profiles".to_owned()),
                    params: Vec::new(),
                    detail: "confirm the exact frequency/bandwidth/antenna-mask tuple on this radio"
                        .to_owned(),
                }];
                steps.push(SilvusApiStep {
                    effect: SilvusStepEffect::SoftBoot,
                    method: Some("freq_bw".to_owned()),
                    params: vec![
                        format_frequency(self.network.center_frequency_mhz),
                        self.network.bandwidth_mhz.api_value().to_owned(),
                    ],
                    detail: "set center frequency and bandwidth atomically; the radio services soft-boot"
                        .to_owned(),
                });
                steps.push(SilvusApiStep {
                    effect: SilvusStepEffect::WaitForReconnect,
                    method: None,
                    params: Vec::new(),
                    detail: "wait for the JSON-RPC service and mesh status to recover before issuing another call"
                        .to_owned(),
                });
                steps.extend([
                    api_step("nw_name", vec![self.network.network_id.clone()]),
                    api_step(
                        "max_link_distance",
                        vec![assessment.resolved_link_distance_m.to_string()],
                    ),
                    api_step(
                        "routing_beacon_period",
                        vec![self.network.routing_beacon_period_ms.to_string()],
                    ),
                    api_step("tx_ant_mask", vec![group.antenna_mask.to_string()]),
                    api_step("rx_ant_mask", vec![group.antenna_mask.to_string()]),
                    api_step(
                        "beamform_enable",
                        vec![if group.beamforming { "1" } else { "0" }.to_owned()],
                    ),
                ]);
                match group.transmit_power {
                    TransmitPowerMode::MaxSupported => {
                        steps.push(api_step("enable_max_power", vec!["1".to_owned()]));
                    }
                    TransmitPowerMode::TargetDbm { dbm } => {
                        steps.push(api_step("enable_max_power", vec!["0".to_owned()]));
                        steps.push(api_step("power_dBm", vec![dbm.to_string()]));
                    }
                }
                steps.push(SilvusApiStep {
                    effect: SilvusStepEffect::Verify,
                    method: Some("print_all_settings".to_owned()),
                    params: Vec::new(),
                    detail: "compare effective values with the Arc-owned desired generation"
                        .to_owned(),
                });
                if self.network.encryption_required {
                    steps.push(SilvusApiStep {
                        effect: SilvusStepEffect::Verify,
                        method: Some("enc_disable".to_owned()),
                        params: Vec::new(),
                        detail: "verify RF encryption is enabled without reading or publishing key material"
                            .to_owned(),
                    });
                    steps.push(SilvusApiStep {
                        effect: SilvusStepEffect::Verify,
                        method: Some("enc_profile".to_owned()),
                        params: Vec::new(),
                        detail: "verify the effective encryption profile without reading or publishing key material"
                            .to_owned(),
                    });
                }
                if self.persist_after_apply {
                    let mut settings = vec![
                        "freq",
                        "bw",
                        "nw_name",
                        "max_link_distance",
                        "routing_beacon_period",
                        "tx_ant_mask",
                        "rx_ant_mask",
                        "beamform_enable",
                        "enable_max_power",
                    ];
                    if matches!(group.transmit_power, TransmitPowerMode::TargetDbm { .. }) {
                        settings.push("power_dBm");
                    }
                    for setting in settings {
                        steps.push(SilvusApiStep {
                            effect: SilvusStepEffect::Persist,
                            method: Some("setenvlinsingle".to_owned()),
                            params: vec![setting.to_owned()],
                            detail: format!("persist the verified current {setting} value to flash"),
                        });
                    }
                }
                Ok(SilvusGroupApplyTemplate {
                    group_id: group.group_id.clone(),
                    regulatory_profile: group.regulatory_profile,
                    maximum_conducted_power_per_port_dbm: group
                        .regulatory_profile
                        .max_conducted_power_per_port_dbm(self.network.bandwidth_mhz),
                    requires_password_authenticated_api: true,
                    requires_live_supported_frequency_profile: true,
                    steps,
                })
            })
            .collect()
    }
}

fn api_step(method: &str, params: Vec<String>) -> SilvusApiStep {
    SilvusApiStep {
        effect: SilvusStepEffect::Live,
        method: Some(method.to_owned()),
        params,
        detail: format!("set {method} on this radio"),
    }
}

fn format_frequency(value: f64) -> String {
    let value = format!("{value:.1}");
    value.trim_end_matches('0').trim_end_matches('.').to_owned()
}
