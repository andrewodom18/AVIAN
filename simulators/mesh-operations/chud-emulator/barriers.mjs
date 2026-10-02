const PHASES = new Set(["request_accepted", "side_effect_recorded", "before_response"]);

// Test-only rendezvous. Each arm pauses one mutation; controls remain responsive.
export class Barriers {
  constructor(timeoutMs = 10000) {
    this.timeoutMs = timeoutMs;
    this.armed = null;
    this.pending = new Map();
    this.sequence = 0;
  }

  arm(phase) {
    if (!PHASES.has(phase)) throw Object.assign(new Error("invalid barrier phase"), { status: 400 });
    if (this.armed || this.pending.size) throw Object.assign(new Error("barrier already active"), { status: 409 });
    this.armed = phase;
  }

  async reach(phase, request, response) {
    if (this.armed !== phase) return;
    this.armed = null;
    const id = `barrier-${++this.sequence}`;
    await new Promise((resolve, reject) => {
      const finish = (error) => {
        clearTimeout(timer);
        response.off("close", aborted);
        this.pending.delete(id);
        if (error) reject(error); else resolve();
      };
      const aborted = () => finish(Object.assign(new Error("barrier caller disconnected"), { status: 499 }));
      const timer = setTimeout(() => finish(Object.assign(new Error("barrier deadline exceeded"), { status: 504 })), this.timeoutMs);
      response.once("close", aborted);
      this.pending.set(id, { phase, route: request.url.split("?")[0], finish });
    });
  }

  release(id) {
    const barrier = this.pending.get(id);
    if (!barrier) throw Object.assign(new Error("barrier not found"), { status: 404 });
    barrier.finish();
  }

  close() {
    for (const barrier of this.pending.values()) barrier.finish(Object.assign(new Error("emulator stopped"), { status: 503 }));
    this.armed = null;
  }

  snapshot() {
    return { armed: this.armed, pending: [...this.pending].map(([id, { phase, route }]) => ({ id, phase, route })) };
  }
}
