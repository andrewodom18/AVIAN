import { createHash, timingSafeEqual } from "node:crypto";

const DEFAULT_DEVICES = [
  { mac: "00:1e:3f:20:9a:10", radio_type: "silvus", label: "AVIAN Radio 1" },
  { mac: "20:9b:60:20:9a:11", radio_type: "trellisware", label: "AVIAN Radio 2" },
];

const DEFAULT_CONFIG = {
  network_id: { value: "AVIAN-DEMO" },
  preset: { value: "MISSION" },
  center_frequency_mhz: { value: 2450 },
  bandwidth_mhz: { value: 20 },
  transmit_power_dbm: { value: 20 },
};

const DEFAULT_META = Object.fromEntries(
  Object.keys(DEFAULT_CONFIG).map((field) => [field, {
    writable: true,
    restorable: true,
    reboot_required: field === "center_frequency_mhz" || field === "bandwidth_mhz",
  }]),
);

export function canonicalMac(value) {
  const compact = String(value ?? "").replace(/[^0-9a-f]/gi, "").toLowerCase();
  if (!/^[0-9a-f]{12}$/.test(compact)) throw new Error("invalid MAC address");
  return compact.match(/.{2}/g).join(":");
}

function clone(value) {
  return structuredClone(value);
}

export class ChudEmulator {
  constructor({ token = "", devices = DEFAULT_DEVICES } = {}) {
    this.token = token;
    this.devices = new Map();
    for (const device of devices) {
      const mac = canonicalMac(device.mac);
      this.devices.set(mac, {
        ...device,
        mac,
        state: device.state ?? "connected",
        reach_addr: device.reach_addr ?? "10.1.0.2",
        reach_iface: device.reach_iface ?? "sim-loopback",
        driver_available: device.driver_available ?? true,
        config: clone(device.config ?? DEFAULT_CONFIG),
        meta: clone(device.meta ?? DEFAULT_META),
      });
    }
    this.operations = new Map();
    this.ledger = [];
    this.fault = null;
    this.sequence = 0;
  }

  authorize(header) {
    if (!this.token) return true;
    const expected = Buffer.from(`Bearer ${this.token}`);
    const actual = Buffer.from(String(header ?? ""));
    return actual.length === expected.length && timingSafeEqual(actual, expected);
  }

  setFault(fault) {
    const supported = new Set([
      null, "apply_error", "apply_timeout", "missing_operation_id", "readback_mismatch",
      "operation_expired", "reboot_required", "stale_device", "authentication_failed",
    ]);
    if (!supported.has(fault)) throw new Error(`unsupported fault: ${fault}`);
    this.fault = fault;
    return this.controlState();
  }

  controlState() {
    return { simulated: true, hardware_write: false, fault: this.fault };
  }

  listDevices() {
    return {
      devices: [...this.devices.values()].map((device) => {
        const state = this.fault === "stale_device" ? "stale"
          : this.fault === "authentication_failed" ? "authentication_failed"
            : device.state;
        const { config: _config, meta: _meta, ...summary } = device;
        return { ...summary, state };
      }),
      simulated: true,
      hardware_write: false,
    };
  }

  snapshot(mac) {
    const device = this.requireDevice(mac);
    const config = clone(device.config);
    if (this.fault === "readback_mismatch") config.network_id = { value: "FAULT-INJECTED" };
    return {
      mac: device.mac,
      vendor: device.radio_type,
      config,
      meta: clone(device.meta),
      simulated: true,
      hardware_write: false,
    };
  }

  apply(payload) {
    if (this.fault === "apply_error") throw Object.assign(new Error("fault-injected apply failure"), { status: 502 });
    if (this.fault === "apply_timeout") throw Object.assign(new Error("fault-injected apply timeout"), { status: 504 });
    const device = this.requireDevice(payload?.mac);
    if (!payload?.desired || typeof payload.desired !== "object" || Array.isArray(payload.desired)) {
      throw Object.assign(new Error("desired configuration is required"), { status: 400 });
    }
    for (const field of Object.keys(payload.desired)) {
      if (!device.meta[field]?.writable) {
        throw Object.assign(new Error(`field is not writable: ${field}`), { status: 400 });
      }
    }
    const operationId = `sim-op-${String(++this.sequence).padStart(4, "0")}`;
    const prior = clone(device.config);
    device.config = { ...device.config, ...clone(payload.desired) };
    const operation = {
      operation_id: operationId,
      mac: device.mac,
      desired: clone(payload.desired),
      prior,
      awaiting_confirmation: true,
      done: false,
      error: null,
      result: null,
      simulated: true,
      hardware_write: false,
    };
    if (this.fault === "operation_expired") {
      device.config = prior;
      operation.awaiting_confirmation = false;
      operation.done = true;
      operation.result = { rolled_back: true, state: "rolled_back" };
    }
    this.operations.set(operationId, operation);
    this.ledger.push({ sequence: this.sequence, action: "apply", operation_id: operationId, mac: device.mac, desired: clone(payload.desired) });
    return {
      operation_id: this.fault === "missing_operation_id" ? "" : operationId,
      awaiting_confirmation: operation.awaiting_confirmation,
      reboot_required: this.fault === "reboot_required" || Object.keys(payload.desired).some((field) => device.meta[field]?.reboot_required),
      simulated: true,
      hardware_write: false,
    };
  }

  confirm(payload) {
    const operation = this.operations.get(String(payload?.operation_id ?? ""));
    if (!operation) throw Object.assign(new Error("operation not found"), { status: 404 });
    if (operation.result?.rolled_back) throw Object.assign(new Error("operation already rolled back"), { status: 409 });
    operation.awaiting_confirmation = false;
    operation.done = true;
    operation.result = { rolled_back: false, state: "confirmed" };
    this.ledger.push({ sequence: ++this.sequence, action: "confirm", operation_id: operation.operation_id, mac: operation.mac });
    return { ...clone(operation.result), operation_id: operation.operation_id, simulated: true, hardware_write: false };
  }

  listOperations(mac) {
    const filter = mac ? canonicalMac(mac) : null;
    return {
      operations: [...this.operations.values()].filter((operation) => !filter || operation.mac === filter).map(clone),
      simulated: true,
      hardware_write: false,
    };
  }

  getLedger() {
    const digest = createHash("sha256").update(JSON.stringify(this.ledger)).digest("hex");
    return { events: clone(this.ledger), digest, simulated: true, hardware_write: false };
  }

  requireDevice(value) {
    let mac;
    try { mac = canonicalMac(value); } catch (error) { throw Object.assign(error, { status: 400 }); }
    const device = this.devices.get(mac);
    if (!device) throw Object.assign(new Error(`radio not found: ${mac}`), { status: 404 });
    return device;
  }
}
