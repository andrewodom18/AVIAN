import Ajv from "ajv/dist/2020.js";
import { readFileSync } from "node:fs";
import { createHash } from "node:crypto";

const read = (name) => JSON.parse(readFileSync(new URL(`./schemas/${name}`, import.meta.url), "utf8"));
const ajv = new Ajv({ strict: true, allErrors: true });
ajv.addSchema(read("transport-event-v1.schema.json"));
const validate = ajv.compile(read("validation-report-v2.schema.json"));

export function validateReport(report, { requireEvents = true } = {}) {
  if (!validate(report)) throw new Error(`Invalid v2 report: ${ajv.errorsText(validate.errors)}`);
  if (requireEvents && !report.events_included) throw new Error("Full event evidence is required");
  for (const scenario of [...report.scenarios, ...report.fault_scenarios]) {
    const { metrics, messages, events } = scenario;
    if (metrics.nodes !== scenario.config.aircraft + 1 || metrics.messages_attempted !== messages.length
      || metrics.messages_delivered + metrics.messages_dropped !== messages.length) throw new Error("Inconsistent message accounting");
    if (report.events_included) {
      const digest = createHash("sha256").update(JSON.stringify(events)).digest("hex");
      if (digest !== scenario.event_digest_sha256) throw new Error("Event digest mismatch");
      if (events.length === 0) throw new Error("Full report has no event evidence");
      for (let i = 0; i < events.length; i++) {
        if (events[i].sequence !== i + 1 || (i && events[i].at_ms < events[i - 1].at_ms)) throw new Error("Invalid event ordering");
      }
    }
  }
  return report;
}
