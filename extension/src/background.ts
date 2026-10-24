// Service worker: message router, scheduler and run coordinator. It holds no
// secrets: runs seal results to the vault's public key, which needs no passphrase.
import { loadCore } from './core.js';
import { runInTab } from './driver-tab.js';
import { HOST_NAME, hostStatus, runOnHost, sessionCommand } from './host.js';
import { importBatchFiles } from './importer.js';
import * as store from './store.js';
import type { Draft, FieldKind, PageRequest, PickMessage, PickMode, Recipe, RecipeEntry, Runner, RunOutcome, RunRecord } from './types.js';

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

chrome.runtime.onInstalled.addListener(details => {
  void setup();
  // First install: the dashboard, where the vault is created, comes first.
  if (details.reason === 'install') void chrome.runtime.openOptionsPage();
});
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
    case 'approveRecipe': {
      if (!(await store.getRecipes())[m.id]) throw new Error('Unknown recipe');
      await mutateRecipes(recipes => { if (recipes[m.id]) delete recipes[m.id].review; });
      return { ok: true };
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

/** What a recipe opens and clicks: a change to any of these from Claude needs the
 * person's review (it runs with the person's own browser sessions). */
const reach = (r: Recipe) => JSON.stringify([r.url, r.actions, r.workflow, r.nextSelector, r.useNativeHost]);

/** `fromClaude`: save even before the site is allowed (Claude can't grant
 * permissions; the dashboard then offers Allow access), and hold new or redirected
 * recipes for review. Saved by the person, a recipe is reviewed. */
async function saveRecipe(input: unknown, fromClaude = false): Promise<Recipe> {
  const core = await loadCore();
  const json = JSON.stringify(input);
  const recipe = JSON.parse(core.validateRecipe(json)) as Recipe;
  const origin = core.recipeOrigin(json);
  if (!fromClaude && !(await chrome.permissions.contains({ origins: [origin + '/*'] }))) {
    throw new Error(`Allow Pocket Scraper Secure to access ${origin} first.`);
  }
  await mutateRecipes(recipes => {
    const now = new Date().toISOString();
    const previous = recipes[recipe.id];
    const review = fromClaude && (!previous || previous.review === true || reach(previous.recipe) !== reach(recipe));
    recipes[recipe.id] = { recipe, createdAt: previous?.createdAt ?? now, updatedAt: now, needsAttention: previous?.needsAttention ?? false, ...(review ? { review: true } : {}) };
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
async function startRun(id: string, trigger: RunRecord['trigger'], runId: string = crypto.randomUUID()): Promise<string | undefined> {
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
    if (entry.review) return 'Claude created or changed this recipe: review and approve it in the dashboard first.';
    started = true;
    void runRecipe(entry, header, trigger, runId).finally(() => running.delete(id));
    return undefined;
  } finally {
    if (!started) running.delete(id);
  }
}

async function runRecipe(entry: RecipeEntry, header: string, trigger: RunRecord['trigger'], runId: string): Promise<void> {
  const run: RunRecord = { id: runId, recipeId: entry.recipe.id, via: entry.recipe.useNativeHost ? 'host' : 'tab', trigger, startedAt: new Date().toISOString(), status: 'running', warnings: [] };
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

// ── Claude access: the bridge to the MCP server ─────────────────────────────
// Only while the person has turned Claude access on. The extension calls out to the
// native host and keeps that connection open; the host passes on the MCP server's
// commands (from an owner-only mailbox folder) and their answers. Nothing listens.

let bridgePort: chrome.runtime.Port | undefined;
let bridgeRetry = 0;
/** Under the host's 16 MiB limit for messages from the extension. */
const REPLY_MAX = 15 * 1024 * 1024;
let bridgeTimer: ReturnType<typeof setTimeout> | undefined;

async function syncBridge() {
  const { claudeAccess } = await store.getSettings();
  if (claudeAccess && !bridgePort) openBridge();
  if (!claudeAccess && bridgePort) {
    bridgePort.disconnect();
    bridgePort = undefined;
  }
}

function openBridge() {
  clearTimeout(bridgeTimer);
  const port = chrome.runtime.connectNative(HOST_NAME);
  bridgePort = port;
  const parts = new Map<string, string[]>();
  port.onMessage.addListener((m: { type: string; id?: string; part?: number; parts?: number; text?: string }) => {
    if (m.type === 'bridgeReady') { bridgeRetry = 0; return; }
    if (m.type !== 'bridgeRequest' || typeof m.id !== 'string' || typeof m.text !== 'string' || !Number.isInteger(m.part) || !Number.isInteger(m.parts) || m.parts! > 32) return;
    const list = parts.get(m.id) ?? new Array<string>(m.parts!).fill('');
    list[m.part!] = m.text;
    parts.set(m.id, list);
    if (m.part! + 1 < m.parts!) return;
    parts.delete(m.id);
    const id = m.id;
    let request: { command?: unknown; args?: unknown; replyKey?: unknown };
    try { request = JSON.parse(list.join('')); } catch { return; }
    const fail = (error: unknown) => port.postMessage({ type: 'bridgeReply', id, ok: false, error: String((error as Error)?.message ?? error) });
    void bridgeCommand(String(request.command), (request.args ?? {}) as Record<string, unknown>).then(async result => {
      // Sealed to the asking process's one-time key: the mailbox never holds it open.
      const sealed = (await loadCore()).sealReply(String(request.replyKey), id, JSON.stringify(result ?? null));
      if (sealed.length > REPLY_MAX) throw new Error('The answer is too large: ask for fewer records (limit), or use export_records.');
      port.postMessage({ type: 'bridgeReply', id, ok: true, sealed });
    }).catch(fail);
  });
  port.onDisconnect.addListener(() => {
    if (bridgePort === port) bridgePort = undefined;
    // Try again while access stays on (e.g. the host was just installed).
    bridgeTimer = setTimeout(() => void syncBridge(), Math.min(60_000, 1000 * 2 ** bridgeRetry++));
  });
  port.postMessage({ type: 'bridge' });
}

void syncBridge();
chrome.storage.onChanged.addListener(changes => { if (changes.claudeAccess) void syncBridge(); });

/** Asks an open, unlocked dashboard (the only place records can be opened). */
async function viaDashboard(command: string, args: Record<string, unknown>): Promise<unknown> {
  let reply: { ok?: boolean; result?: unknown; error?: string } | undefined;
  try { reply = await chrome.runtime.sendMessage({ type: 'vaultRequest', command, args }); }
  catch { throw new Error('Open the Pocket Scraper Secure dashboard and unlock the vault to allow this.'); }
  if (!reply) throw new Error('Open the Pocket Scraper Secure dashboard and unlock the vault to allow this.');
  if (!reply.ok) throw new Error(reply.error ?? 'The dashboard refused');
  return reply.result;
}

const text = (v: unknown, name: string) => {
  if (typeof v !== 'string' || !v) throw new Error(`${name} is required`);
  return v;
};

async function bridgeCommand(command: string, args: Record<string, unknown>): Promise<unknown> {
  // Checked per command: the bridge may still be closing when access is turned off.
  if (!(await store.getSettings()).claudeAccess) throw new Error('Claude access is off.');
  const core = await loadCore();
  switch (command) {
    case 'status': {
      const header = await store.getVaultHeader();
      let locked = true;
      try { locked = !((await viaDashboard('vaultState', {})) as { unlocked: boolean }).unlocked; } catch { /* no dashboard */ }
      return {
        connected: true,
        vault: header ? { exists: true, locked, fingerprint: core.vaultFingerprint(header) } : { exists: false },
        recipes: Object.keys(await store.getRecipes()).length,
        runners: (await store.getRunners()).length,
        nativeHost: await hostStatus().catch(() => ({ installed: false }))
      };
    }
    case 'getSettings': return store.getSettings();
    case 'updateSettings': {
      const change = { runTimeoutMs: args.runTimeoutMs, autoLockMinutes: args.autoLockMinutes } as Partial<store.Settings>;
      // A longer auto-lock keeps records readable longer: only the person can raise it.
      if (typeof change.autoLockMinutes === 'number' && change.autoLockMinutes > (await store.getSettings()).autoLockMinutes) {
        throw new Error('Only the person can make the vault stay unlocked longer (dashboard). Claude can only shorten it.');
      }
      return store.updateSettings(change);
    }
    case 'listRecipes': {
      const entries = Object.values(await store.getRecipes());
      return Promise.all(entries.map(async e => ({
        ...e.recipe,
        needsAttention: e.needsAttention,
        waitingForReview: e.review === true,
        siteAllowed: await chrome.permissions.contains({ origins: [new URL(e.recipe.url).origin + '/*'] }),
        records: await store.countRecords(e.recipe.id),
        updatedAt: e.updatedAt
      })));
    }
    case 'saveRecipe': {
      const recipe = await saveRecipe(args.recipe, true);
      const allowed = await chrome.permissions.contains({ origins: [new URL(recipe.url).origin + '/*'] });
      const review = (await store.getRecipes())[recipe.id]?.review === true;
      const waits = [review ? 'review and approve it' : '', allowed ? '' : `click Allow access for ${new URL(recipe.url).origin}`].filter(Boolean);
      return { recipe, siteAllowed: allowed, waitingForReview: review, ...(waits.length ? { note: `Before it can run, the person must ${waits.join(' and ')} in the dashboard.` } : {}) };
    }
    case 'deleteRecipe': {
      const id = text(args.id, 'id');
      if (!(await store.getRecipes())[id]) throw new Error('Unknown recipe');
      await mutateRecipes(recipes => { delete recipes[id]; });
      if (args.deleteRecords === true) await store.deleteRecords(id);
      await syncAlarms();
      return { deleted: id, recordsDeleted: args.deleteRecords === true };
    }
    case 'runRecipe': {
      const runId = crypto.randomUUID();
      const problem = await startRun(text(args.id, 'id'), 'manual', runId);
      if (problem) throw new Error(problem);
      return { runId };
    }
    case 'listRuns': {
      const limit = Math.min(200, Math.max(1, Number(args.limit) || 50));
      const runs = await store.listRuns(1000);
      return runs.filter(r => !args.recipeId || r.recipeId === args.recipeId).slice(0, limit);
    }
    case 'readRecords':
    case 'exportRecords':
    case 'exportBundle':
      return viaDashboard(command, args);
    case 'listRunners': return store.getRunners();
    case 'enrollRunner': {
      const key = text(args.key, 'key').trim();
      const fingerprint = core.keyFingerprint(key);
      const name = text(args.name, 'name').trim().slice(0, 60);
      const list = await store.getRunners();
      if (list.some(r => r.key === key)) throw new Error('That runner is already enrolled.');
      // A runner's batches become records: the person approves it in the dashboard.
      const runner: Runner = { name, key, fingerprint, addedAt: new Date().toISOString(), pending: true };
      await store.setRunners([...list, runner]);
      return { ...runner, note: 'Waiting for the person to compare the fingerprint and click Approve under Runners in the dashboard.' };
    }
    case 'removeRunner': {
      const which = text(args.runner, 'runner');
      const list = await store.getRunners();
      const keep = list.filter(r => r.key !== which && r.name !== which);
      if (keep.length === list.length) throw new Error('No such runner');
      await store.setRunners(keep);
      return { removed: list.length - keep.length };
    }
    case 'importBatches': {
      const files = Array.isArray(args.files) ? (args.files as { name: unknown; text: unknown }[]) : [];
      return importBatchFiles(core, files.slice(0, 500).map(f => ({ name: String(f.name), size: String(f.text).length, text: async () => String(f.text) })));
    }
    default:
      throw new Error(`Unknown command ${command}`);
  }
}
