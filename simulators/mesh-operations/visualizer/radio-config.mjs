export const DEFAULT_RADIO_CONFIGURATION = Object.freeze({
  network_id: "AVIAN-DEMO",
  band: "S-BAND",
  center_frequency_mhz: 2440,
  bandwidth_mhz: 20,
  transmit_power_dbm: 20,
  routing_beacon_period_ms: 500,
  encryption_required: true,
});

const supportedBandwidths = new Set([1.25, 2.5, 5, 10, 20, 40]);
const bandRanges = new Map([
  ["UHF-LOW", [225, 450]],
  ["UHF", [698, 970]],
  ["L-BAND", [1250, 1850]],
  ["S-BAND", [1850, 2600]],
  ["C-BAND", [3200, 6000]],
]);

export function validateRadioConfiguration(value) {
  const config = {
    network_id: String(value?.network_id ?? "").trim(),
    band: String(value?.band ?? ""),
    center_frequency_mhz: Number(value?.center_frequency_mhz),
    bandwidth_mhz: Number(value?.bandwidth_mhz),
    transmit_power_dbm: Number(value?.transmit_power_dbm),
    routing_beacon_period_ms: Number(value?.routing_beacon_period_ms),
    encryption_required: Boolean(value?.encryption_required),
  };

  if (!/^[A-Za-z0-9 -]{1,32}$/.test(config.network_id)) {
    throw new Error("Network ID must use 1–32 letters, numbers, spaces, or hyphens.");
  }
  const range = bandRanges.get(config.band);
  if (!range) throw new Error("Select a supported RF band.");
  if (!Number.isFinite(config.center_frequency_mhz)
      || config.center_frequency_mhz < range[0]
      || config.center_frequency_mhz > range[1]
      || Math.abs(config.center_frequency_mhz * 10 - Math.round(config.center_frequency_mhz * 10)) > 1e-6) {
    throw new Error(`Center frequency must be within ${range[0]}–${range[1]} MHz in 0.1 MHz steps.`);
  }
  if (!supportedBandwidths.has(config.bandwidth_mhz)) {
    throw new Error("Channel width must be 1.25, 2.5, 5, 10, 20, or 40 MHz.");
  }
  if (!Number.isInteger(config.transmit_power_dbm)
      || config.transmit_power_dbm < 0
      || config.transmit_power_dbm > 39) {
    throw new Error("Transmit power must be an integer from 0–39 dBm per active RF port.");
  }
  if (!Number.isInteger(config.routing_beacon_period_ms)
      || config.routing_beacon_period_ms < 100
      || config.routing_beacon_period_ms > 2000) {
    throw new Error("Routing beacon period must be 100–2000 ms.");
  }
  return config;
}
