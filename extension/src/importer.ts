// Importing runner result batches, shared by the dashboard (file picker, bucket) and
// the service worker (Claude, through the bridge). Works while the vault is locked:
// records stay sealed; only signatures and the vault's public key are checked.
import type { Core } from './core.js';
import * as store from './store.js';
import type { Batch } from './types.js';

export type BatchFile = { name: string; text: () => Promise<string>; size: number };
/** `handled` were dealt with for good; `deferred` may succeed later (runner not
 * enrolled yet, recipe missing); anything else (e.g. storage failed) is retried. */
export type ImportOutcome = { lines: string[]; handled: string[]; deferred: string[] };

export async function importBatchFiles(core: Core, files: BatchFile[]): Promise<ImportOutcome> {
  const outcome: ImportOutcome = { lines: [], handled: [], deferred: [] };
  const header = await store.getVaultHeader();
  if (!header) {
    outcome.lines.push('Create the vault first.');
    return outcome;
  }
  const all = await store.getRunners();
  const runners = all.filter(r => !r.pending);
  const recipes = await store.getRecipes();
  const now = new Date().toISOString();
  for (const file of files) {
    let batch: Batch;
    try {
      if (file.size > 8 * 1024 * 1024) throw new Error('larger than 8 MB');
      batch = JSON.parse(core.openBatch(await file.text(), JSON.stringify(runners.map(r => r.key)), header)) as Batch;
    } catch (e) {
      const message = (e as Error).message;
      const waiting = /isn't enrolled/.test(message) && all.some(r => r.pending);
      outcome.lines.push(`${file.name}: refused (${message}${waiting ? '; a runner is waiting for approval under Runners' : ''})`);
      // From a runner that isn't enrolled yet: keep it for later. Anything else
      // (tampered, another vault, malformed) never becomes valid.
      (/isn't enrolled/.test(message) ? outcome.deferred : outcome.handled).push(file.name);
      continue;
    }
    if (!recipes[batch.recipeId]) {
      outcome.lines.push(`${file.name}: set aside (its recipe isn't here; remove it from the runner with: pocket-scraper-runner remove ${batch.recipeId})`);
      outcome.deferred.push(file.name);
      continue;
    }
    const runner = runners.find(r => r.key === batch.runnerKey)!;
    // Runner-supplied times can't be in the future (they order and prune run history).
    const clamp = (t: string) => (t > now ? now : t);
    try {
      const result = await store.importBatch(batch.batchId, batch.recipeId, batch.records, {
        id: `batch-${batch.batchId}`, recipeId: batch.recipeId, via: 'runner', runner: runner.name, trigger: 'schedule',
        startedAt: clamp(batch.startedAt), finishedAt: clamp(batch.finishedAt), status: batch.status, rowCount: batch.rowCount,
        warnings: batch.warnings, ...(batch.error ? { error: batch.error } : {})
      });
      outcome.lines.push(result === 'duplicate' ? `${file.name}: already imported` : `${file.name}: ${batch.status.replace('_', ' ')}, ${batch.rowCount} rows from ${runner.name}`);
      outcome.handled.push(file.name);
    } catch (e) {
      outcome.lines.push(`${file.name}: not saved, will retry (${(e as Error).message})`);
    }
  }
  return outcome;
}
