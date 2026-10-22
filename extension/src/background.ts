// Service worker: message router, scheduler and run coordinator. It holds no
// secrets: runs seal results to the vault's public key, which needs no passphrase.
import { loadCore } from './core.js';
import { runInTab } from './driver-tab.js';
import { hostStatus, runOnHost, sessionCommand } from './host.js';
import * as store from './store.js';
import type { Draft, FieldKind, PageRequest, PickMessage, PickMode, Recipe, RecipeEntry, RunOutcome, RunRecord } from './types.js';

const MAX_CONCURRENT_RUNS = 2;
const ALARM_PREFIX = 'recipe:';
/** Recipes with a run in progress. Reserved synchronously, before any await. */
const running = new Set<string>();

// Keep extension storage away from content scripts (they run in web page processes).
// Every service-worker start, not only install: the setting is not guaranteed to stick.
const restricted = Promise.all([chrome.storage.local, chrome.storage.session].map(area =>
  area.setAccessLevel({ accessLevel: 'TRUSTED_CONTEXTS' }).then(() => true, (error: unknown) => {
    console.error('Could not restrict storage access:', error);
    return false;
  })));

chrome.runtime.onInstalled.addListener(() => void setup());
chrome.runtime.onStartup.addListener(() => void setup());

async function setup() {
  await store.closeStaleRuns();
  await syncAlarms();
}

/** Serialises every change to the recipes map, so concurrent updates can't undo each other. */
let recipeQueue: Promise<unknown> = Promise.resolve();
function mutateRecipes<T>(change: (recipes: Record<string, RecipeEntry>) => T | Promise<T>): Promise<T> {
  const next = recipeQueue.then(async () => {
    const recipes = await store.getRecipes();
    const result = await change(recipes);
    await store.setRecipes(recipes);
    return result;
  });
  recipeQueue = next.catch(() => {});
  return next;
}

/**
 * Creates alarms that are missing or whose period changed, and removes alarms of
 * deleted or unscheduled recipes. Existing alarms are left alone, so saving one
 * recipe or restarting Chrome never pushes the others' next run back. A recreated
 * alarm resumes from the recipe's last run.
 */
async function syncAlarms() {
  const recipes = await store.getRecipes();
  const alarms = new Map((await chrome.alarms.getAll()).filter(a => a.name.startsWith(ALARM_PREFIX)).map(a => [a.name, a]));
  for (const { recipe } of Object.values(recipes)) {
    const name = ALARM_PREFIX + recipe.id;
    const existing = alarms.get(name);
    alarms.delete(name);
    if (!recipe.scheduleMinutes) {
      if (existing) await chrome.alarms.clear(name);
      continue;
    }
    if (existing?.periodInMinutes === recipe.scheduleMinutes) continue;
    const period = recipe.scheduleMinutes * 60_000;
    const last = await store.lastRunStart(recipe.id);
    const when = Math.max(Date.now() + 60_000, last === undefined ? Date.now() + period : last + period);
    await chrome.alarms.create(name, { when, periodInMinutes: recipe.scheduleMinutes });
  }
  for (const orphan of alarms.keys()) await chrome.alarms.clear(orphan);
}

chrome.alarms.onAlarm.addListener(alarm => {
  if (alarm.name.startsWith(ALARM_PREFIX)) void startRun(alarm.name.slice(ALARM_PREFIX.length), 'schedule');
});

chrome.runtime.onMessage.addListener((message, sender, sendResponse) => {
  if (sender.id !== chrome.runtime.id) return false;
  const fromPage = typeof sender.url === 'string' && sender.url.startsWith(chrome.runtime.getURL(''));
  const handled = fromPage ? handlePage(message as PageRequest) : sender.tab ? handlePick(message as PickMessage, sender) : undefined;
  if (!handled) return false;
  handled.then(sendResponse, (error: unknown) => sendResponse({ error: String((error as Error)?.message ?? error) }));
  return true;
});

async function handlePage(m: PageRequest): Promise<unknown> {
  switch (m?.type) {
    case 'startPicker':
      return startPicker(m.tabId, m.mode, m.fieldName, m.kind);
    case 'saveRecipe':
      return saveRecipe(m.recipe);
    case 'deleteRecipe': {
      await mutateRecipes(recipes => { delete recipes[m.id]; });
      if (m.purgeRecords === true) await store.deleteRecords(m.id);
      await syncAlarms();
      return { ok: true };
    }
    case 'runNow': {
      // Answer once the run has started; progress shows in the dashboard's run list.
      const problem = await startRun(m.id, 'manual');
      if (problem) throw new Error(problem);
      return { ok: true };
    }
    case 'hostStatus':
      return hostStatus();
    case 'storageRestricted':
      return { restricted: (await restricted).every(Boolean) };
    case 'session': {
      const entry = (await store.getRecipes())[m.id];
      if (!entry) throw new Error('Unknown recipe');
      const result = await sessionCommand(m.action, entry.recipe.url);
      if (result.ok && m.action === 'close') await setAttention(entry.recipe.url, false);
      return result;
    }
    case 'clearAttention': {
      const entry = (await store.getRecipes())[m.id];
      if (!entry) throw new Error('Unknown recipe');
      await setAttention(entry.recipe.url, false);
      return { ok: true };
    }
    default:
      throw new Error('Unknown request');
  }
}

async function saveRecipe(input: unknown): Promise<Recipe> {
  const core = await loadCore();
  const json = JSON.stringify(input);
  const recipe = JSON.parse(core.validateRecipe(json)) as Recipe;
  const origin = core.recipeOrigin(json);
  if (!(await chrome.permissions.contains({ origins: [origin + '/*'] }))) {
    throw new Error(`Allow Pocket Scraper Secure to access ${origin} first.`);
  }
  await mutateRecipes(recipes => {
    const now = new Date().toISOString();
    const previous = recipes[recipe.id];
    recipes[recipe.id] = { recipe, createdAt: previous?.createdAt ?? now, updatedAt: now, needsAttention: previous?.needsAttention ?? false };
  });
  await syncAlarms();
  return recipe;
}

async function setAttention(url: string, needsAttention: boolean) {
  const origin = new URL(url).origin;
  await mutateRecipes(recipes => {
    for (const entry of Object.values(recipes)) if (new URL(entry.recipe.url).origin === origin) entry.needsAttention = needsAttention;
  });
}

/**
 * Starts a run unless something prevents it; returns the reason, or undefined once
 * the run is under way. The slot is reserved before the first await, so a click
 * racing an alarm can't start the same recipe twice or exceed the limit.
 */
async function startRun(id: string, trigger: RunRecord['trigger']): Promise<string | undefined> {
  if (running.has(id)) return 'This recipe is already running.';
  if (running.size >= MAX_CONCURRENT_RUNS) return 'Too many runs in progress. Wait for one to finish.';
  running.add(id);
  let started = false;
  try {
    const entry = (await store.getRecipes())[id];
    if (!entry) return 'Unknown recipe';
    const header = await store.getVaultHeader();
    if (!header) return 'Set up the vault in the dashboard first: results are only ever stored encrypted.';
    if (entry.needsAttention && trigger === 'schedule') return 'Paused: the site needs attention.';
    started = true;
    void runRecipe(entry, header, trigger).finally(() => running.delete(id));
    return undefined;
  } finally {
    if (!started) running.delete(id);
  }
}

async function runRecipe(entry: RecipeEntry, header: string, trigger: RunRecord['trigger']): Promise<void> {
  const run: RunRecord = { id: crypto.randomUUID(), recipeId: entry.recipe.id, via: entry.recipe.useNativeHost ? 'host' : 'tab', trigger, startedAt: new Date().toISOString(), status: 'running', warnings: [] };
  try {
    await store.putRun(run);
    const core = await loadCore();
    // Storage is not trusted: re-validate the recipe and re-check site access.
    const json = JSON.stringify(entry.recipe);
    const recipe = JSON.parse(core.validateRecipe(json)) as Recipe;
    const origin = core.recipeOrigin(json);
    if (!(await chrome.permissions.contains({ origins: [origin + '/*'] }))) throw new Error(`Access to ${origin} was removed. Save the recipe again to allow it.`);
    const { runTimeoutMs } = await store.getSettings();
    const observedAt = new Date().toISOString();
    let outcome: RunOutcome;
    if (recipe.useNativeHost) {
      outcome = await runOnHost(core, recipe, header, run.id, observedAt, runTimeoutMs);
    } else {
      const result = await runInTab(core, recipe, runTimeoutMs);
      outcome = result.ok
        ? { ok: true, records: JSON.parse(core.sealRows(header, recipe.id, run.id, observedAt, JSON.stringify(result.rows))), rowCount: result.rows.length, warnings: result.warnings }
        : result;
    }
    if (outcome.ok) {
      await store.addRecords(recipe.id, run.id, outcome.records);
      run.rowCount = outcome.rowCount;
      run.warnings = outcome.warnings;
      run.status = 'success';
    } else {
      run.status = outcome.attention ? 'needs_attention' : 'error';
      run.error = outcome.message;
      if (outcome.attention) await setAttention(recipe.url, true);
    }
  } catch (error) {
    run.status = 'error';
    run.error = String((error as Error)?.message ?? error);
  } finally {
    run.finishedAt = new Date().toISOString();
    await store.putRun(run).catch(() => {});
  }
}

// Picker: one session at a time, bound to a tab and a one-time nonce.

const PICK_MODES: PickMode[] = ['row', 'field', 'action', 'next'];
const KINDS: FieldKind[] = ['text', 'href', 'html', 'value'];
interface PickerSession { tabId: number; nonce: string; mode: PickMode; fieldName: string; kind: FieldKind }

async function startPicker(tabId: number, mode: PickMode, fieldName: string, kind: FieldKind) {
  if (!Number.isInteger(tabId) || !PICK_MODES.includes(mode) || !KINDS.includes(kind) || typeof fieldName !== 'string' || fieldName.length > 80) {
    throw new Error('Invalid picker request');
  }
  const draft = await store.getDraft();
  const session: PickerSession = { tabId, nonce: crypto.randomUUID(), mode, fieldName: fieldName.trim(), kind };
  await chrome.storage.session.set({ picker: session });
  await chrome.scripting.executeScript({ target: { tabId }, files: ['picker.js'] });
  await chrome.scripting.executeScript({
    target: { tabId },
    func: (options: unknown) => (globalThis as unknown as { __pssPicker?: (o: unknown) => void }).__pssPicker?.(options),
    args: [{ mode, nonce: session.nonce, rowSelector: mode === 'field' ? draft.rowSelector ?? '' : '', kind }]
  });
  return { ok: true };
}

async function handlePick(m: PickMessage, sender: chrome.runtime.MessageSender): Promise<unknown> {
  const { picker } = (await chrome.storage.session.get('picker')) as { picker?: PickerSession };
  if (!picker || m?.type !== 'pick' || m.nonce !== picker.nonce || sender.tab?.id !== picker.tabId || sender.frameId !== 0) {
    throw new Error('Ignored');
  }
  await chrome.storage.session.remove('picker');
  if (typeof m.selector !== 'string' || !m.selector.trim() || m.selector.length > 500 || /[\u0000-\u001f\u007f]/.test(m.selector)) throw new Error('Invalid selection');
  const matches = Number.isInteger(m.matches) ? m.matches : 0;
  const draft: Draft = await store.getDraft();
  if (picker.mode === 'field') {
    const name = picker.fieldName || `field_${draft.fields.length + 1}`;
    const field = { name, selector: m.selector, kind: picker.kind };
    const existing = draft.fields.findIndex(f => f.name === name);
    if (existing >= 0) draft.fields[existing] = field;
    else draft.fields.push(field);
  } else if (picker.mode === 'row') {
    draft.rowSelector = m.selector;
  } else if (picker.mode === 'action') {
    if (draft.workflow) draft.workflow.steps.push({ type: 'click', selector: m.selector, waitMs: 700 });
    else draft.actions.push({ selector: m.selector, waitMs: 700 });
  } else {
    draft.nextSelector = m.selector;
    if ((draft.maxPages ?? 1) === 1) draft.maxPages = 5;
  }
  draft.lastPick = `${picker.mode}: ${m.selector} (${matches} match${matches === 1 ? '' : 'es'})`;
  await store.setDraft(draft);
  return { ok: true };
}
