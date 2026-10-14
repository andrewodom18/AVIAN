import { createHash, timingSafeEqual } from "node:crypto";
import { isDeepStrictEqual } from "node:util";
import { stateStore } from "./state-store.mjs";

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

const failure = (message, status = 400) => Object.assign(new Error(message), { status });
const FAULTS = new Set([
  null, "apply_error", "apply_timeout", "missing_operation_id", "readback_mismatch",
  "operation_expired", "reboot_required", "stale_device", "authentication_failed",
  "accepted_response_lost",
]);
const STATES = new Set(["connected", "unplugged", "rebooting", "stale", "authentication_failed"]);

export class ChudEmulator {
  constructor({ token = "", devices = DEFAULT_DEVICES, stateFile, now = Date.now, maxOperations = 1000, maxEvents = 10000 } = {}) {
    this.token = token;
    this.now = now;
    this.maxOperations = maxOperations;
    this.maxEvents = maxEvents;
    this.store = stateFile ? stateStore(stateFile) : null;
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
    this.clockOffset = 0;
    this.deviceFaults = new Map();
    const saved = this.store?.load();
    if (saved) this.restore(saved);
    this.expire();
  }

  exportState() {
    return clone({ devices: [...this.devices], operations: [...this.operations], ledger: this.ledger,
      fault: this.fault, deviceFaults: [...this.deviceFaults], sequence: this.sequence, clockOffset: this.clockOffset });
  }

  restore(state) {
    if (!state || !Array.isArray(state.devices) || !Array.isArray(state.operations)
      || !Array.isArray(state.ledger) || !Array.isArray(state.deviceFaults)
      || !Number.isSafeInteger(state.sequence) || state.sequence < 0
      || !Number.isSafeInteger(state.clockOffset) || state.clockOffset < 0
      || !FAULTS.has(state.fault) || state.operations.length > this.maxOperations
      || state.ledger.length > this.maxEvents) throw new Error("invalid emulator state");
    const devices = new Map(state.devices);
    const operations = new Map(state.operations);
    if (devices.size !== state.devices.length || operations.size !== state.operations.length) throw new Error("duplicate persisted identity");
    for (const [mac, device] of devices) {
      if (canonicalMac(mac) !== mac || device.mac !== mac || !STATES.has(device.state)
        || !device.config || !device.meta) throw new Error("invalid persisted device");
    }
    for (const [id, operation] of operations) {
      if (operation.operation_id !== id || !devices.has(operation.mac)
        || !Number.isSafeInteger(operation.deadline_ms) || !operation.prior || !operation.desired
        || typeof operation.awaiting_confirmation !== "boolean" || typeof operation.done !== "boolean"
        || operation.deadline_ms < 0 || operation.simulated !== true || operation.hardware_write !== false
        || operation.error !== null) throw new Error("invalid persisted operation");
    }
    // A checksum proves byte integrity, not a complete or coherent transaction history.
    if (state.sequence !== state.ledger.length) throw new Error("invalid persisted ledger sequence");
    const histories = new Map();
    const configs = new Map();
    const active = new Map();
    const object = (value) => value !== null && typeof value === "object" && !Array.isArray(value);
    for (const [index, event] of state.ledger.entries()) {
      const operation = operations.get(event?.operation_id);
      if (!operation || event.sequence !== index + 1 || event.mac !== operation.mac) throw new Error("invalid persisted ledger identity");
      const previous = histories.get(event.operation_id);
      if (event.action === "apply") {
        if (previous || active.has(event.mac) || event.operation_id !== `sim-op-${String(event.sequence).padStart(4, "0")}`
          || !object(operation.prior) || !object(operation.desired) || !Object.keys(operation.desired).length
          || !isDeepStrictEqual(event.desired, operation.desired)
          || (configs.has(event.mac) && !isDeepStrictEqual(configs.get(event.mac), operation.prior))) throw new Error("invalid persisted apply history");
        configs.set(event.mac, { ...operation.prior, ...operation.desired });
        active.set(event.mac, event.operation_id);
      } else {
        if (previous !== "apply" || active.get(event.mac) !== event.operation_id
          || !["confirm", "rollback"].includes(event.action)) throw new Error("invalid persisted terminal history");
        if (event.action === "rollback") {
          if (event.cause !== "confirmation_expired") throw new Error("invalid persisted rollback cause");
          configs.set(event.mac, operation.prior);
        }
        active.delete(event.mac);
      }
      histories.set(event.operation_id, event.action);
    }
    for (const [id, operation] of operations) {
      const action = histories.get(id);
      const pending = action === "apply";
      const result = pending ? null : { rolled_back: action === "rollback", state: action === "rollback" ? "rolled_back" : "confirmed" };
      if (!action || operation.awaiting_confirmation !== pending || operation.done === pending
        || !isDeepStrictEqual(operation.result, result)) throw new Error("invalid persisted operation history");
    }
    for (const [mac, config] of configs) {
      if (!isDeepStrictEqual(devices.get(mac).config, config)) throw new Error("invalid persisted configuration history");
    }
    if (new Map(state.deviceFaults).size !== state.deviceFaults.length) throw new Error("duplicate persisted fault");
    for (const [mac, fault] of state.deviceFaults) {
      if (!devices.has(mac) || !FAULTS.has(fault)) throw new Error("invalid persisted fault");
    }
    this.devices = devices;
    this.operations = operations;
    this.ledger = state.ledger;
    this.fault = state.fault;
    this.deviceFaults = new Map(state.deviceFaults);
    this.sequence = state.sequence;
    this.clockOffset = state.clockOffset;
  }

  mutate(action) {
    const prior = this.exportState();
    try {
      const result = action();
      this.store?.save(this.exportState());
      return result;
    } catch (error) {
      this.restore(prior);
      throw error;
    }
  }

  timestamp() { return this.now() + this.clockOffset; }
  faultFor(mac) { return this.deviceFaults.get(canonicalMac(mac)) ?? this.fault; }

  record(action, operation, extra = {}) {
    if (this.ledger.length >= this.maxEvents) throw failure("emulator ledger capacity reached", 507);
    this.ledger.push({ sequence: ++this.sequence, action, operation_id: operation.operation_id,
      mac: operation.mac, ...extra });
  }

  expire() {
    if (![...this.operations.values()].some((op) => op.awaiting_confirmation && op.deadline_ms <= this.timestamp())) return;
    this.mutate(() => {
      for (const operation of this.operations.values()) {
        if (operation.awaiting_confirmation && operation.deadline_ms <= this.timestamp()) this.rollback(operation);
      }
    });
  }

  rollback(operation) {
    this.devices.get(operation.mac).config = clone(operation.prior);
    operation.awaiting_confirmation = false;
    operation.done = true;
    operation.result = { rolled_back: true, state: "rolled_back" };
    this.record("rollback", operation, { cause: "confirmation_expired" });
  }

  advance(milliseconds) {
    if (!Number.isSafeInteger(milliseconds) || milliseconds < 0 || milliseconds > 3_600_000) throw failure("invalid clock advance");
    this.mutate(() => { this.clockOffset += milliseconds; });
    this.expire();
    return this.controlState();
  }

  setDevice(payload) {
    if (!payload || typeof payload !== "object" || Array.isArray(payload)) throw failure("invalid device control");
    const { mac, state, driver_available, meta } = payload;
    const device = this.requireDevice(mac);
    if (state !== undefined && !STATES.has(state)) throw failure("invalid device state");
    if (driver_available !== undefined && typeof driver_available !== "boolean") throw failure("invalid driver state");
    if (meta !== undefined && (!meta || typeof meta !== "object" || Array.isArray(meta)
      || Object.entries(meta).some(([key, value]) => !Object.hasOwn(device.meta, key) || !value
        || Object.entries(value).some(([name, flag]) => !["writable", "restorable", "reboot_required"].includes(name) || typeof flag !== "boolean")))) throw failure("invalid capability metadata");
    this.mutate(() => {
      if (state !== undefined) device.state = state;
      if (driver_available !== undefined) device.driver_available = driver_available;
      if (meta) for (const [key, value] of Object.entries(meta)) device.meta[key] = { ...device.meta[key], ...value };
    });
    return this.controlState();
  }

  authorize(header) {
    if (!this.token) return true;
    const expected = Buffer.from(`Bearer ${this.token}`);
    const actual = Buffer.from(String(header ?? ""));
    return actual.length === expected.length && timingSafeEqual(actual, expected);
  }

  setFault(fault, mac) {
    if (!FAULTS.has(fault)) throw failure(`unsupported fault: ${fault}`);
    if (mac) this.requireDevice(mac);
    this.mutate(() => {
      if (mac) {
        if (fault === null) this.deviceFaults.delete(canonicalMac(mac));
        else this.deviceFaults.set(canonicalMac(mac), fault);
      } else this.fault = fault;
    });
    return this.controlState();
  }

  controlState() {
    return { simulated: true, hardware_write: false, fault: this.fault,
      device_faults: Object.fromEntries(this.deviceFaults), clock_offset_ms: this.clockOffset };
  }

  listDevices() {
    this.expire();
    return {
      devices: [...this.devices.values()].filter((device) => !["unplugged", "rebooting"].includes(device.state)).map((device) => {
        const fault = this.faultFor(device.mac);
        const state = fault === "stale_device" ? "stale"
          : fault === "authentication_failed" ? "authentication_failed"
            : device.state;
        const { config: _config, meta: _meta, ...summary } = device;
        return { ...summary, state };
      }),
      simulated: true,
      hardware_write: false,
    };
  }

  snapshot(mac) {
    this.expire();
    const device = this.availableDevice(mac);
    const config = clone(device.config);
    if (this.faultFor(mac) === "readback_mismatch") config.network_id = { value: "FAULT-INJECTED" };
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
    this.expire();
    const device = this.availableDevice(payload?.mac);
    const fault = this.faultFor(device.mac);
    if (fault === "apply_error") throw failure("fault-injected apply failure", 502);
    if (fault === "apply_timeout") throw failure("fault-injected apply timeout", 504);
    if (this.operations.size >= this.maxOperations) throw failure("emulator operation capacity reached", 507);
    if ([...this.operations.values()].some((op) => op.mac === device.mac && op.awaiting_confirmation)) throw failure("device already has an unconfirmed operation", 409);
    if (!payload?.desired || typeof payload.desired !== "object" || Array.isArray(payload.desired)) {
      throw Object.assign(new Error("desired configuration is required"), { status: 400 });
    }
    if (Object.keys(payload.desired).length === 0) throw failure("desired configuration is empty");
    const timeout = payload.confirm_timeout_seconds ?? 30;
    if (!Number.isSafeInteger(timeout) || timeout < 1 || timeout > 3600) throw failure("invalid confirmation timeout");
    for (const [field, setting] of Object.entries(payload.desired)) {
      if (!Object.hasOwn(device.meta, field) || !device.meta[field]?.writable || !device.meta[field]?.restorable) {
        throw Object.assign(new Error(`field is not writable: ${field}`), { status: 400 });
      }
      if (!setting || typeof setting !== "object" || Array.isArray(setting) || !Object.hasOwn(setting, "value")) throw failure(`invalid setting: ${field}`);
    }
    return this.mutate(() => {
      const operationId = `sim-op-${String(this.sequence + 1).padStart(4, "0")}`;
      const prior = clone(device.config);
      device.config = { ...device.config, ...clone(payload.desired) };
      const operation = {
        operation_id: operationId,
        mac: device.mac,
        desired: clone(payload.desired),
        prior,
        deadline_ms: this.timestamp() + timeout * 1000,
        awaiting_confirmation: true,
        done: false,
        error: null,
        result: null,
        simulated: true,
        hardware_write: false,
      };
      this.operations.set(operationId, operation);
      this.record("apply", operation, { desired: clone(payload.desired) });
      if (fault === "operation_expired") this.rollback(operation);
      return {
        operation_id: fault === "missing_operation_id" ? "" : operationId,
        awaiting_confirmation: operation.awaiting_confirmation,
        reboot_required: fault === "reboot_required" || Object.keys(payload.desired).some((field) => device.meta[field]?.reboot_required),
        simulated: true,
        hardware_write: false,
      };
    });
  }

  confirm(payload) {
    this.expire();
    const operation = this.operations.get(String(payload?.operation_id ?? ""));
    if (!operation) throw Object.assign(new Error("operation not found"), { status: 404 });
    if (operation.result?.rolled_back) throw Object.assign(new Error("operation already rolled back"), { status: 409 });
    if (operation.done) return { ...clone(operation.result), operation_id: operation.operation_id, simulated: true, hardware_write: false };
    this.availableDevice(operation.mac);
    return this.mutate(() => {
      operation.awaiting_confirmation = false;
      operation.done = true;
      operation.result = { rolled_back: false, state: "confirmed" };
      this.record("confirm", operation);
      return { ...clone(operation.result), operation_id: operation.operation_id, simulated: true, hardware_write: false };
    });
  }

  listOperations(mac) {
    this.expire();
    const filter = mac ? canonicalMac(mac) : null;
    return {
      operations: [...this.operations.values()].filter((operation) => !filter || operation.mac === filter).map(clone),
      simulated: true,
      hardware_write: false,
    };
  }

  getLedger() {
    this.expire();
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

  availableDevice(value) {
    const device = this.requireDevice(value);
    const fault = this.faultFor(device.mac);
    if (device.state === "authentication_failed" || fault === "authentication_failed") throw failure("device authentication failed", 401);
    if (device.state !== "connected" || fault === "stale_device") throw failure("device unavailable", 503);
    if (!device.driver_available) throw failure("device driver unavailable", 422);
    return device;
  }
}
