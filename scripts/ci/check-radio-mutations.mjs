import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { pathToFileURL } from 'node:url';

// cargo-mutants can finish without a surviving mutant when every generated
// mutant is unbuildable. That is not evidence of exercised viable mutations.
export function qualifyMutations(mutants, report) {
  if (!Array.isArray(mutants) || mutants.length === 0 || !Array.isArray(report?.outcomes)) {
    throw new Error('Mutation evidence must contain generated mutants and outcomes');
  }
  const baselines = report.outcomes.filter(({ scenario }) => scenario === 'Baseline');
  if (baselines.length !== 1 || baselines[0].summary !== 'Success'
      || !['Build', 'Test'].every((phase) => baselines[0].phase_results?.some(
        (result) => result.phase === phase && result.process_status === 'Success',
      ))) {
    throw new Error('One passing unmodified build/test baseline is required');
  }
  const results = report.outcomes.filter(({ scenario }) => scenario && typeof scenario === 'object' && scenario.Mutant);
  const expected = mutants.map(({ name }) => name);
  const actual = results.map(({ scenario }) => scenario.Mutant.name);
  if (expected.some((name) => typeof name !== 'string' || name.length === 0)
      || new Set(expected).size !== expected.length || new Set(actual).size !== actual.length
      || actual.length !== expected.length || expected.some((name) => !actual.includes(name))
      || report.outcomes.length !== results.length + 1) {
    throw new Error('Every generated mutant must have exactly one recognized outcome');
  }
  const counts = { caught: 0, unviable: 0 };
  for (const { summary } of results) {
    if (summary === 'CaughtMutant') counts.caught += 1;
    else if (summary === 'Unviable') counts.unviable += 1;
    else throw new Error(`Mutation result is not qualified: ${summary}`);
  }
  if (counts.caught === 0) throw new Error('No viable mutant was caught');
  return { generated: expected.length, ...counts };
}

if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) {
  try {
    if (process.argv.length !== 3) throw new Error('Usage: check-radio-mutations.mjs <mutants.out directory>');
    const directory = resolve(process.argv[2]);
    const mutants = JSON.parse(readFileSync(resolve(directory, 'mutants.json'), 'utf8'));
    const outcomes = JSON.parse(readFileSync(resolve(directory, 'outcomes.json'), 'utf8'));
    console.log(JSON.stringify(qualifyMutations(mutants, outcomes)));
  } catch (error) {
    console.error(`Mutation qualification failed: ${error.message}`);
    process.exitCode = 1;
  }
}
