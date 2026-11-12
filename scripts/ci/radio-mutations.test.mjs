import assert from 'node:assert/strict';
import { test } from 'node:test';
import { qualifyMutations } from './check-radio-mutations.mjs';

const generated = [{ name: 'vendor mutant A' }, { name: 'vendor mutant B' }];
function report(summaries = ['CaughtMutant', 'CaughtMutant']) {
  return { outcomes: [
    { scenario: 'Baseline', summary: 'Success', phase_results: [
      { phase: 'Build', process_status: 'Success' }, { phase: 'Test', process_status: 'Success' },
    ] },
    ...summaries.map((summary, index) => ({ scenario: { Mutant: generated[index] }, summary })),
  ] };
}

test('qualifies a passing baseline and all caught viable mutants', () => {
  assert.deepEqual(qualifyMutations(generated, report()), { generated: 2, caught: 2, unviable: 0 });
});
test('reports unviable mutants while requiring exercised viable mutants', () => {
  assert.deepEqual(qualifyMutations(generated, report(['CaughtMutant', 'Unviable'])), { generated: 2, caught: 1, unviable: 1 });
});
for (const state of ['MissedMutant', 'Timeout', 'Failure', 'unknown']) {
  test(`rejects ${state} even alongside a caught mutant`, () => {
    assert.throws(() => qualifyMutations(generated, report(['CaughtMutant', state])));
  });
}
test('rejects unviable-only and empty runs', () => {
  assert.throws(() => qualifyMutations(generated, report(['Unviable', 'Unviable'])));
  assert.throws(() => qualifyMutations([], { outcomes: [] }));
});
test('rejects absent, failed, duplicate or incomplete baselines', () => {
  const absent = report(); absent.outcomes.shift();
  const failed = report(); failed.outcomes[0].summary = 'Failure';
  const duplicate = report(); duplicate.outcomes.push(duplicate.outcomes[0]);
  const incomplete = report(); incomplete.outcomes[0].phase_results.pop();
  for (const evidence of [absent, failed, duplicate, incomplete]) {
    assert.throws(() => qualifyMutations(generated, evidence));
  }
});
test('rejects partial, duplicated, foreign and unknown outcome sets', () => {
  const partial = report(); partial.outcomes.pop();
  const duplicate = report(); duplicate.outcomes[2] = duplicate.outcomes[1];
  const foreign = report(); foreign.outcomes[2].scenario = { Mutant: { name: 'unrelated mutant' } };
  const unknown = report(); unknown.outcomes.push({ scenario: 'Unexpected', summary: 'Success' });
  for (const evidence of [partial, duplicate, foreign, unknown]) {
    assert.throws(() => qualifyMutations(generated, evidence));
  }
});
