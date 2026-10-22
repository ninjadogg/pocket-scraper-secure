// Local persistence. Records are stored sealed; nothing here ever sees plaintext.
// Everything read back is re-validated by its consumer, because storage is not
// trusted (another process could have edited the profile on disk).
import type { Draft, RecipeEntry, RunRecord, SealedRecord, StoredRecord } from './types.js';

const DB_NAME = 'pocket-scraper-secure';
const RUNS_KEPT = 1000;

let dbPromise: Promise<IDBDatabase> | undefined;
function db(): Promise<IDBDatabase> {
  dbPromise ??= new Promise((resolve, reject) => {
    const open = indexedDB.open(DB_NAME, 1);
    open.onupgradeneeded = () => {
      const d = open.result;
      d.createObjectStore('records', { keyPath: ['recipeId', 'key'] }).createIndex('recipeId', 'recipeId');
      d.createObjectStore('runs', { keyPath: 'id' }).createIndex('startedAt', 'startedAt');
    };
    open.onsuccess = () => resolve(open.result);
    open.onerror = () => reject(open.error);
  });
  return dbPromise;
}

function done(tx: IDBTransaction): Promise<void> {
  return new Promise((resolve, reject) => {
    tx.oncomplete = () => resolve();
    tx.onerror = () => reject(tx.error);
    tx.onabort = () => reject(tx.error);
  });
}
function result<T>(request: IDBRequest<T>): Promise<T> {
  return new Promise((resolve, reject) => {
    request.onsuccess = () => resolve(request.result);
    request.onerror = () => reject(request.error);
  });
}

/** Stores a run's sealed records. Keys are random, so duplicates are only found and
 * removed after unlocking (see deleteRecordKeys). */
export async function addRecords(recipeId: string, runId: string, records: SealedRecord[]): Promise<void> {
  const tx = (await db()).transaction('records', 'readwrite');
  const store = tx.objectStore('records');
  for (const r of records) store.add({ recipeId, runId, key: r.key, sealed: r.sealed } satisfies StoredRecord);
  await done(tx);
}

export async function deleteRecordKeys(recipeId: string, keys: string[]): Promise<void> {
  if (!keys.length) return;
  const tx = (await db()).transaction('records', 'readwrite');
  const store = tx.objectStore('records');
  for (const key of keys) store.delete([recipeId, key]);
  await done(tx);
}

export async function listRecords(recipeId: string): Promise<StoredRecord[]> {
  const tx = (await db()).transaction('records');
  return result(tx.objectStore('records').index('recipeId').getAll(recipeId));
}

export async function countRecords(recipeId: string): Promise<number> {
  const tx = (await db()).transaction('records');
  return result(tx.objectStore('records').index('recipeId').count(recipeId));
}

export async function deleteRecords(recipeId: string): Promise<void> {
  const tx = (await db()).transaction('records', 'readwrite');
  const store = tx.objectStore('records');
  for (const key of await result(store.index('recipeId').getAllKeys(recipeId))) store.delete(key);
  await done(tx);
}

export async function putRun(run: RunRecord): Promise<void> {
  const tx = (await db()).transaction('runs', 'readwrite');
  const store = tx.objectStore('runs');
  store.put(run);
  // Keep the newest RUNS_KEPT runs.
  const count = await result(store.count());
  if (count > RUNS_KEPT) {
    let extra = count - RUNS_KEPT;
    const cursor = store.index('startedAt').openCursor();
    await new Promise<void>((resolve, reject) => {
      cursor.onsuccess = () => {
        const c = cursor.result;
        if (!c || extra-- <= 0) return resolve();
        c.delete();
        c.continue();
      };
      cursor.onerror = () => reject(cursor.error);
    });
  }
  await done(tx);
}

/** When this recipe's newest run started (ms), if any. */
export async function lastRunStart(recipeId: string): Promise<number | undefined> {
  const runs = (await listRuns(1000)).filter(r => r.recipeId === recipeId);
  return runs.length ? Date.parse(runs[0].startedAt) : undefined;
}

export async function listRuns(limit = 100): Promise<RunRecord[]> {
  const tx = (await db()).transaction('runs');
  const all = await result(tx.objectStore('runs').index('startedAt').getAll());
  return (all as RunRecord[]).reverse().slice(0, limit);
}

/** Marks runs left `running` by a stopped service worker as interrupted. */
export async function closeStaleRuns(): Promise<void> {
  const tx = (await db()).transaction('runs', 'readwrite');
  const store = tx.objectStore('runs');
  for (const run of (await result(store.getAll())) as RunRecord[]) {
    if (run.status === 'running') store.put({ ...run, status: 'error', finishedAt: new Date().toISOString(), error: 'Interrupted: the browser stopped the extension before this run finished.' });
  }
  await done(tx);
}

// chrome.storage.local: vault header (wrapped key), recipes, draft, settings.

export async function getVaultHeader(): Promise<string | undefined> {
  const { vault } = await chrome.storage.local.get('vault');
  return typeof vault === 'string' ? vault : undefined;
}
export async function setVaultHeader(header: string): Promise<void> {
  await chrome.storage.local.set({ vault: header });
}

export async function getRecipes(): Promise<Record<string, RecipeEntry>> {
  const { recipes } = await chrome.storage.local.get('recipes');
  return recipes && typeof recipes === 'object' ? (recipes as Record<string, RecipeEntry>) : {};
}
export async function setRecipes(recipes: Record<string, RecipeEntry>): Promise<void> {
  await chrome.storage.local.set({ recipes });
}

export function emptyDraft(): Draft {
  return { fields: [], actions: [], maxPages: 1, scheduleMinutes: 0, rowSelector: '', nextSelector: '', workflow: null, useNativeHost: false };
}
export async function getDraft(): Promise<Draft> {
  const { draft } = await chrome.storage.local.get('draft');
  return draft && typeof draft === 'object' && Array.isArray((draft as Draft).fields) ? (draft as Draft) : emptyDraft();
}
export async function setDraft(draft: Draft): Promise<void> {
  await chrome.storage.local.set({ draft });
}

export interface Settings { runTimeoutMs: number; autoLockMinutes: number }
export async function getSettings(): Promise<Settings> {
  const { settings } = await chrome.storage.local.get('settings');
  const s = (settings ?? {}) as Partial<Settings>;
  const n = (v: unknown, min: number, max: number, fallback: number) => (Number.isInteger(v) && (v as number) >= min && (v as number) <= max ? (v as number) : fallback);
  return { runTimeoutMs: n(s.runTimeoutMs, 10_000, 3_600_000, 180_000), autoLockMinutes: n(s.autoLockMinutes, 1, 240, 10) };
}
