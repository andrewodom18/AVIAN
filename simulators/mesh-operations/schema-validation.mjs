import Ajv from "ajv/dist/2020.js";
import { readFileSync } from "node:fs";
import { createHash } from "node:crypto";

const read = (name) => JSON.parse(readFileSync(new URL(`./schemas/${name}`, import.meta.url), "utf8"));
const ajv = new Ajv({ strict: true, allErrors: true });
ajv.addSchema(read("transport-event-v1.schema.json"));
const validate = ajv.compile(read("validation-report-v2.schema.json"));

const sizes = [5, 25, 50, 100, 150, 200];
const profiles = ["recovery", "congestion", "loss", "duplication", "expiry"];
const demand = (condition, detail) => { if (!condition) throw new Error(`Inconsistent report: ${detail}`); };
const faultOutcomes = new Set(["node_offline", "node_restored", "link_offline", "link_restored"]);
const delivered = new Set(["delivered", "stale_generation_rejected"]);
const dropped = new Set(["ttl_expired", "retry_exhausted"]);

export function validateReport(report, { requireEvents = true } = {}) {
  if (!validate(report)) throw new Error(`Invalid v2 report: ${ajv.errorsText(validate.errors)}`);
  if (requireEvents && !report.events_included) throw new Error("Full event evidence is required");
  const scenarios = [...report.scenarios, ...report.fault_scenarios];
  demand(report.passed === scenarios.every((s) => s.passed), "aggregate verdict");
  demand(new Set(scenarios.map((s) => s.config.seed)).size === 1, "mixed matrix seeds");
  const keys = new Set(scenarios.map((s) => `${s.config.aircraft}/${s.config.profile}`));
  demand(keys.size === 30 && sizes.every((size) => profiles.every((profile) => keys.has(`${size}/${profile}`))), "matrix coverage");
  demand(report.scenarios.every((s) => s.config.profile === "recovery")
    && report.fault_scenarios.every((s) => s.config.profile !== "recovery"), "matrix grouping");
  for (const scenario of scenarios) {
    const { metrics, messages, events, config, links, faults } = scenario;
    const nodes = config.aircraft + 1;
    const node = (id) => Number.isSafeInteger(id) && id >= 0 && id < nodes;
    demand(messages.length > 0 && metrics.nodes === nodes && metrics.messages_attempted === messages.length
      && metrics.messages_delivered + metrics.messages_dropped === messages.length, "message accounting");
    demand(Math.abs(metrics.delivery_ratio - metrics.messages_delivered / messages.length) < 1e-12, "delivery ratio");
    const byId = new Map(messages.map((m) => [m.id, m]));
    demand(byId.size === messages.length && messages.every((m) => node(m.source) && node(m.target)), "message identities");
    const degrees = Array(nodes).fill(0), pairs = new Set();
    for (const link of links) {
      demand(node(link.left) && node(link.right), "link identity");
      degrees[link.left]++; degrees[link.right]++;
      pairs.add(`${Math.min(link.left, link.right)}/${Math.max(link.left, link.right)}`);
    }
    demand(metrics.links === links.length && metrics.max_degree === Math.max(...degrees)
      && metrics.self_links === links.filter((l) => l.left === l.right).length
      && metrics.duplicate_links === links.length - pairs.size, "graph metrics");
    demand(faults.every((f) => node(f.node) && (f.link_peer === null || (node(f.link_peer) && f.node !== f.link_peer))), "fault identity");
    demand(metrics.latest_generation_converged === (metrics.converged_nodes === nodes) && metrics.converged_nodes <= nodes, "convergence count");
    const generationTwoAt = metrics.generation_two_convergence_ms === null ? null : metrics.generation_two_convergence_ms + 5000;
    demand(metrics.recovery_ms === (generationTwoAt !== null && generationTwoAt >= 10000 ? generationTwoAt - 10000 : null), "recovery timing");
    if (scenario.passed) {
      const invariant = metrics.max_degree <= config.direct_peer_limit && metrics.self_links === 0
        && metrics.duplicate_links === 0 && metrics.peak_queue <= config.transport.queue_capacity
        && metrics.commands_applied <= config.aircraft;
      const recovered = metrics.generation_one_convergence_ms !== null && metrics.generation_one_convergence_ms < 5000
        && metrics.recovery_ms !== null && metrics.recovery_ms < 5000 && metrics.latest_generation_converged;
      const accepted = {
        recovery: recovered && metrics.messages_dropped <= metrics.partitioned_nodes && metrics.ttl_expired === 0,
        duplication: recovered && metrics.duplicate_rejected > 0 && metrics.commands_applied === config.aircraft,
        congestion: metrics.queue_drops > 0 && metrics.messages_dropped > metrics.partitioned_nodes,
        loss: metrics.messages_delivered === 0 && metrics.retry_exhausted === messages.length && metrics.commands_applied === 0,
        expiry: metrics.messages_delivered === 0 && metrics.ttl_expired === messages.length && metrics.commands_applied === 0,
      };
      demand(invariant && accepted[config.profile], "scenario PASS contradicts acceptance metrics");
    }
    if (report.events_included) {
      const digest = createHash("sha256").update(JSON.stringify(events)).digest("hex");
      if (digest !== scenario.event_digest_sha256) throw new Error("Event digest mismatch");
      demand(events.length > 0, "missing event evidence");
      const terminals = new Map(), counts = new Map(), latencies = [];
      const faultEvents = [];
      let commands = 0;
      for (const [i, event] of events.entries()) {
        demand(event.sequence === i + 1 && (!i || event.at_ms >= events[i - 1].at_ms), "event ordering");
        demand(node(event.from) && node(event.to), "event node identity");
        counts.set(event.outcome, (counts.get(event.outcome) ?? 0) + 1);
        if (faultOutcomes.has(event.outcome)) {
          demand(event.message_id === 0 && event.generation === 0 && event.attempt === 0, "fault event fields");
          const link = event.outcome.startsWith("link_");
          demand(link ? event.from !== event.to : event.from === event.to, "fault event endpoints");
          faultEvents.push({ at_ms: event.at_ms, node: event.from, link_peer: link ? event.to : null, online: event.outcome.endsWith("restored") });
          continue;
        }
        const message = byId.get(event.message_id);
        demand(message && message.generation === event.generation && event.at_ms >= message.created_ms
          && event.attempt <= config.transport.retry_limit, "event message reference");
        if (delivered.has(event.outcome) || dropped.has(event.outcome)) {
          demand(!terminals.has(message.id) && event.to === message.target, "terminal message outcome");
          terminals.set(message.id, event.outcome);
          if (delivered.has(event.outcome)) {
            demand(event.at_ms < message.created_ms + config.transport.ttl_ms, "delivery after TTL");
            latencies.push(event.at_ms - message.created_ms);
            if (event.outcome === "delivered" && message.command) commands++;
          }
        } else if (terminals.has(message.id)) {
          demand(event.outcome === "duplicate_rejected", "activity after terminal outcome");
        }
      }
      const faultKey = (f) => `${f.at_ms}/${f.node}/${f.link_peer}/${f.online}`;
      demand(JSON.stringify(faultEvents.map(faultKey).sort()) === JSON.stringify(faults.map(faultKey).sort()), "fault event coverage");
      demand(terminals.size === messages.length && latencies.length === metrics.messages_delivered
        && messages.length - latencies.length === metrics.messages_dropped, "terminal accounting");
      for (const [metric, outcome] of Object.entries({ queue_drops: "queue_full", ttl_expired: "ttl_expired", retry_exhausted: "retry_exhausted", duplicate_rejected: "duplicate_rejected", stale_generation_rejected: "stale_generation_rejected" })) {
        demand(metrics[metric] === (counts.get(outcome) ?? 0), `${metric} counter`);
      }
      latencies.sort((a, b) => a - b);
      const percentile = (p) => latencies.length ? latencies[Math.ceil((latencies.length - 1) * p / 100)] : 0;
      demand(metrics.delivery_latency_p50_ms === percentile(50) && metrics.delivery_latency_p95_ms === percentile(95), "latency metrics");
      demand(metrics.commands_applied === commands, "command accounting");
    } else {
      demand(events.length === 0, "summary contains undeclared events");
    }
  }
  return report;
}
