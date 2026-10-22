// Service worker: message router, scheduler and run coordinator. It holds no
// secrets: runs seal results to the vault's public key, which needs no passphrase.
import { loadCore } from './core.js';
import { runInTab } from './driver-tab.js';
import { hostStatus, runOnHost, sessionCommand } from './host.js';
import * as store from './store.js';
import type { Draft, FieldKind, PageRequest, PickMessage, PickMode, Recipe, RecipeEntry, RunOutcome, RunRecord } from './types.js';

const MAX_CONCURRENT_RUNS = 2;
const ALARM_PREFIX = 'recipe:';
const running = new Set<string>();

chrome.runtime.onInstalled.addListener(() => void setup());
chrome.runtime.onStartup.addListener(() => void setup());

async function setup() {
  // Keep extension storage away from content scripts (they run in web page processes).
  for (const area of [chrome.storage.local, chrome.storage.session]) {
    await area.setAccessLevel({ accessLevel: 'TRUSTED_CONTEXTS' }).catch(() => {});
  }
  await store.closeStaleRuns();
  await syncAlarms();
}

async function syncAlarms() {
  const recipes = await store.getRecipes();
  for (const alarm of await chrome.alarms.getAll()) {
    if (alarm.name.startsWith(ALARM_PREFIX)) await chrome.alarms.clear(alarm.name);
  }
  for (const { recipe } of Object.values(recipes)) {
    if (recipe.scheduleMinutes > 0) {
      await chrome.alarms.create(ALARM_PREFIX + recipe.id, { delayInMinutes: recipe.scheduleMinutes, periodInMinutes: recipe.scheduleMinutes });
    }
  }
}

chrome.alarms.onAlarm.addListener(alarm => {
  if (alarm.name.startsWith(ALARM_PREFIX)) void runRecipe(alarm.name.slice(ALARM_PREFIX.length), 'schedule').catch(() => {});
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
      const recipes = await store.getRecipes();
      delete recipes[m.id];
      await store.setRecipes(recipes);
      if (m.purgeRecords === true) await store.deleteRecords(m.id);
      await syncAlarms();
      return { ok: true };
    }
    case 'runNow': {
      // Answer at once; the run's progress shows in the dashboard's run list.
      const problem = await runBlocker(m.id, 'manual');
      if (problem) throw new Error(problem);
      void runRecipe(m.id, 'manual').catch(() => {});
      return { ok: true };
    }
    case 'hostStatus':
      return hostStatus();
    case 'session': {
      const entry = (await store.getRecipes())[m.id];
      if (!entry) throw new Error('Unknown recipe');
      const result = await sessionCommand(m.action, entry.recipe.url);
      if (result.ok && m.action === 'close') await clearAttention(entry.recipe.url);
      return result;
    }
    case 'clearAttention': {
      const entry = (await store.getRecipes())[m.id];
      if (!entry) throw new Error('Unknown recipe');
      await clearAttention(entry.recipe.url);
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
  const recipes = await store.getRecipes();
  const now = new Date().toISOString();
  const previous = recipes[recipe.id];
  recipes[recipe.id] = { recipe, createdAt: previous?.createdAt ?? now, updatedAt: now, needsAttention: previous?.needsAttention ?? false };
  await store.setRecipes(recipes);
  await syncAlarms();
  return recipe;
}

async function clearAttention(url: string) {
  const origin = new URL(url).origin;
  const recipes = await store.getRecipes();
  for (const entry of Object.values(recipes)) if (new URL(entry.recipe.url).origin === origin) entry.needsAttention = false;
  await store.setRecipes(recipes);
}

/** Why a run can't start now, or undefined. */
async function runBlocker(id: string, trigger: RunRecord['trigger']): Promise<string | undefined> {
  const entry = (await store.getRecipes())[id];
  if (!entry) return 'Unknown recipe';
  if (!(await store.getVaultHeader())) return 'Set up the vault in the dashboard first: results are only ever stored encrypted.';
  if (running.has(id)) return 'This recipe is already running.';
  if (running.size >= MAX_CONCURRENT_RUNS) return 'Too many runs in progress. Wait for one to finish.';
  if (entry.needsAttention && trigger === 'schedule') return 'Paused: the site needs attention.';
  return undefined;
}

async function runRecipe(id: string, trigger: RunRecord['trigger']): Promise<void> {
  if (await runBlocker(id, trigger)) return;
  const entry = (await store.getRecipes())[id] as RecipeEntry;
  const header = (await store.getVaultHeader()) as string;
  const core = await loadCore();
  running.add(id);
  const run: RunRecord = { id: crypto.randomUUID(), recipeId: id, via: entry.recipe.useNativeHost ? 'host' : 'tab', trigger, startedAt: new Date().toISOString(), status: 'running', warnings: [] };
  try {
    await store.putRun(run);
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
      run.newCount = await store.addRecords(recipe.id, run.id, observedAt, outcome.records);
      run.rowCount = outcome.rowCount;
      run.warnings = outcome.warnings;
      run.status = 'success';
    } else {
      run.status = outcome.attention ? 'needs_attention' : 'error';
      run.error = outcome.message;
      if (outcome.attention) {
        const recipes = await store.getRecipes();
        for (const e of Object.values(recipes)) if (new URL(e.recipe.url).origin === origin) e.needsAttention = true;
        await store.setRecipes(recipes);
      }
    }
  } catch (error) {
    run.status = 'error';
    run.error = String((error as Error)?.message ?? error);
  } finally {
    running.delete(id);
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
