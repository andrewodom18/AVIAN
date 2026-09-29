import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import Ajv from "ajv/dist/2020.js";
import { validateReport } from "./schema-validation.mjs";

const schema = JSON.parse(readFileSync(new URL("./schemas/transport-event-v1.schema.json", import.meta.url)));
test("event contract rejects missing, extra, mistyped and unknown fields", () => {
  const validate = new Ajv({ strict: true }).compile(schema);
  const valid = { sequence: 1, at_ms: 0, message_id: 1, from: 0, to: 1, generation: 1, attempt: 0, outcome: "queued" };
  assert.equal(validate(valid), true);
  for (const mutation of [{ ...valid, at_ms: -1 }, { ...valid, attempt: "0" }, { ...valid, outcome: "magic" }, { ...valid, extra: true }]) assert.equal(validate(mutation), false);
  for (const key of Object.keys(valid)) {
    const partial = { ...valid }; delete partial[key];
    assert.equal(validate(partial), false);
  }
});

test("report consumer refuses v1 evidence and incomplete v2 reports", () => {
  for (const version of ["1.0.0", "2.0.0", "3.0.0"]) {
    assert.throws(() => validateReport({ schema_version: version, passed: true }), /Invalid v2 report/);
  }
});
