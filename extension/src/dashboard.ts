// Dashboard: the only place records are decrypted. The unlocked key lives in this
// page's WebAssembly memory and is freed (zeroized) on lock, on inactivity and when
// the page closes.
import type { UnlockedVault } from '../build/core/pocket_scraper_core_wasm.js';
import { loadCore, type Core } from './core.js';
import { $, download, h, send } from './dom.js';
import { ackBatches, collectBatches } from './host.js';
import { importBatchFiles, type BatchFile, type ImportOutcome } from './importer.js';
import * as store from './store.js';
import { startTour, tourOnce, type TourStep } from './tour.js';
import type { PlainRecord, Recipe, RecipeEntry, RunRecord } from './types.js';

let core: Core;
let vault: UnlockedVault | null = null;
let lockTimer: ReturnType<typeof setTimeout> | undefined;
let selected: string | null = null;
let host: { installed: boolean; version?: string } = { installed: false };

function say(text: string, bad = false) {
  const m = $('message');
  m.textContent = text;
  m.classList.toggle('bad', bad);
}

// ── tabs ─────────────────────────────────────────────────────────────────────

const TABS = ['recipes', 'records', 'runners', 'settings'] as const;
type Tab = (typeof TABS)[number];

function showTab(tab: Tab) {
  for (const t of TABS) {
    $(`tab-${t}`).setAttribute('aria-selected', String(t === tab));
    $(`panel-${t}`).hidden = t !== tab;
  }
  history.replaceState(null, '', `#${tab}`);
}

// ── small helpers ────────────────────────────────────────────────────────────

const when = (iso: string) => new Date(iso).toLocaleString(undefined, { dateStyle: 'medium', timeStyle: 'short' });

function ago(iso: string): string {
  const s = Math.round((Date.now() - new Date(iso).getTime()) / 1000);
  if (s < 60) return 'just now';
  if (s < 3600) return `${Math.floor(s / 60)} min ago`;
  if (s < 86400) return `${Math.floor(s / 3600)} h ago`;
  return when(iso);
}

const statusBadge = (status: string) => h('span', { className: `badge status-${status}` }, status.replace('_', ' '));

/** A link only for http(s) addresses; anything else (scraped, so untrusted) stays text. */
function link(url: string, text = url): Node {
  return /^https?:\/\//i.test(url) ? h('a', { href: url, target: '_blank', rel: 'noopener noreferrer', title: url }, text) : document.createTextNode(url);
}

function empty(title: string, text: string): HTMLElement {
  return h('div', { className: 'empty' }, h('strong', {}, title), text);
}

const fileName = (name: string) => name.replace(/[^\w.-]+/g, '_').slice(0, 60) || 'records';

// ── vault ────────────────────────────────────────────────────────────────────

function lock() {
  vault?.free();
  vault = null;
  view = null;
  clearTimeout(lockTimer);
  void renderVault();
  void renderRecipes();
  void renderRecords();
  void renderSettings();
}

/** The fingerprint runners pin (shown with `pocket-scraper-runner add`). */
function fingerprintLine(header: string): HTMLElement[] {
  const fp = core.vaultFingerprint(header);
  return fp ? [h('p', { className: 'small' }, 'Vault fingerprint: ', h('code', {}, fp))] : [];
}

async function touch() {
  if (!vault) return;
  clearTimeout(lockTimer);
  const { autoLockMinutes } = await store.getSettings();
  lockTimer = setTimeout(lock, autoLockMinutes * 60_000);
}

async function renderVault() {
  const header = await store.getVaultHeader();
  const box = $('vault');
  if (!header) {
    const pass = h('input', { type: 'password', autocomplete: 'new-password', placeholder: `At least ${core.minPassphraseChars()} characters` });
    const again = h('input', { type: 'password', autocomplete: 'new-password', placeholder: 'Repeat passphrase' });
    box.replaceChildren(
      h('strong', {}, 'Create the vault'),
      h('p', { className: 'small' }, 'Records are encrypted as they are collected and can only be read with this passphrase. There is no recovery: if you forget it, stored records are lost.'),
      h('div', { className: 'row' }, pass, again, h('button', { className: 'primary tight', onclick: async () => {
        if (pass.value !== again.value) return say('The passphrases differ.', true);
        try {
          say('Creating the vault…');
          await store.setVaultHeader(core.createVault(pass.value));
          pass.value = again.value = '';
          say('Vault created. Unlock it to read records.');
          await renderVault();
        } catch (e) { say((e as Error).message, true); }
      } }, 'Create')));
    return;
  }
  if (!vault) {
    const pass = h('input', { type: 'password', autocomplete: 'current-password', placeholder: 'Passphrase' });
    const unlock = async () => {
      try {
        say('Unlocking…');
        // Always the stored header: it may have changed since this form was drawn.
        const current = (await store.getVaultHeader()) ?? header;
        vault = core.UnlockedVault.unlock(current, pass.value);
        pass.value = '';
        // Vaults made before runners existed get their signing key recorded now.
        if (!core.vaultFingerprint(current)) await store.setVaultHeader(vault.completeHeader(current));
        void renderRecipes();
        void renderSettings();
        say('');
        await touch();
        await renderVault();
        if (selected) await showRecords(selected, false);
      } catch (e) { say((e as Error).message, true); }
    };
    pass.addEventListener('keydown', e => { if (e.key === 'Enter') void unlock(); });
    box.replaceChildren(h('strong', {}, 'Vault locked'), ...fingerprintLine(header), h('div', { className: 'row' }, pass, h('button', { className: 'primary tight', onclick: unlock }, 'Unlock')));
    return;
  }
  const { autoLockMinutes } = await store.getSettings();
  const fp = core.vaultFingerprint((await store.getVaultHeader()) ?? header);
  box.replaceChildren(h('div', { className: 'vault-bar' },
    h('strong', { className: 'status-success' }, 'Vault unlocked'),
    fp ? h('span', { className: 'small' }, 'Fingerprint ', h('code', {}, fp)) : null,
    h('span', { className: 'grow small' }, `Locks after ${autoLockMinutes} min idle.`),
    h('button', { className: 'tight', onclick: lock }, 'Lock now')));
}

// ── recipes and runs ─────────────────────────────────────────────────────────

async function renderRecipes() {
  const recipes = await store.getRecipes();
  host = await send<{ installed: boolean; version?: string }>({ type: 'hostStatus' }).catch(() => ({ installed: false }));
  $('host').textContent = host.installed ? `Native host ${host.version} installed.` : 'Native host not installed: runs use a background tab in this browser.';
  $('collect').hidden = !host.installed;
  $('retryDeferred').hidden = !host.installed;
  const lastRun = new Map<string, RunRecord>();
  for (const run of await store.listRuns(500)) if (!lastRun.has(run.recipeId)) lastRun.set(run.recipeId, run);
  const sorted = Object.values(recipes).sort((a, b) => a.recipe.name.localeCompare(b.recipe.name));
  const rows = await Promise.all(sorted.map(async e =>
    recipeRow(e, await store.countRecords(e.recipe.id), lastRun.get(e.recipe.id), await chrome.permissions.contains({ origins: [new URL(e.recipe.url).origin + '/*'] }))));
  $('recipes').replaceChildren(
    h('tr', {}, ...['Recipe', 'Schedule', 'Runs in', 'Last run', 'Records', ''].map(t => h('th', {}, t))),
    ...(rows.length ? rows : [h('tr', {}, h('td', { colSpan: 6 }, empty('No recipes yet', 'Open a website, click the Pocket Scraper Secure toolbar button and follow the four steps.')))]));
  renderRecipePicker(sorted.map(e => e.recipe));
}

function recipeRow(entry: RecipeEntry, records: number, last: RunRecord | undefined, siteAllowed: boolean): HTMLElement {
  const r = entry.recipe;
  const act = (label: string, fn: () => Promise<unknown>, className = '') => h('button', { className, onclick: async () => { try { await fn(); } catch (e) { say((e as Error).message, true); } } }, label);
  const actions = h('div', { className: 'actions' },
    act('Run now', async () => { await send({ type: 'runNow', id: r.id }); say(`Running ${r.name}…`); }),
    act('Records', () => showRecords(r.id)));
  if (!siteAllowed) {
    // Saved by Claude (which can't grant permissions): one click from the person does.
    const origin = new URL(r.url).origin;
    actions.prepend(act('Allow access', async () => {
      if (await chrome.permissions.request({ origins: [origin + '/*'] })) { say(`Access to ${origin} allowed.`); await renderRecipes(); }
    }, 'primary'));
  }
  if (vault) {
    actions.append(act('Export for runner', async () => {
      const { runTimeoutMs } = await store.getSettings();
      const header = await store.getVaultHeader();
      const file = `${fileName(r.name)}.pssbundle`;
      download(file, 'application/json', vault!.exportBundle(JSON.stringify(r), new Date().toISOString(), runTimeoutMs));
      say(`Exported ${file}. On the runner: pocket-scraper-runner add ${file} --trust ${header ? core.vaultFingerprint(header) : ''}`);
    }));
  }
  if (entry.needsAttention) {
    if (r.useNativeHost && host.installed) {
      actions.append(act('Open session', async () => { await send({ type: 'session', id: r.id, action: 'open' }); say('Sign in or complete verification in the window that opened, then click Finish session.'); }),
        act('Finish session', async () => { await send({ type: 'session', id: r.id, action: 'close' }); say('Session saved; schedules resume.'); await renderRecipes(); }));
    } else {
      actions.append(act('I’ve signed in', async () => { await send({ type: 'clearAttention', id: r.id }); await renderRecipes(); }));
    }
  } else if (r.useNativeHost && host.installed) {
    actions.append(act('Session setup', async () => { await send({ type: 'session', id: r.id, action: 'open' }); say('Sign in in the window that opened, then click Finish session.'); await renderRecipes(); }),
      act('Finish session', async () => { await send({ type: 'session', id: r.id, action: 'close' }); say('Session saved.'); }));
  }
  actions.append(act('Delete', async () => {
    if (!confirm(`Delete “${r.name}”? Its encrypted records are deleted too.`)) return;
    await send({ type: 'deleteRecipe', id: r.id, purgeRecords: true });
    if (selected === r.id) { selected = null; view = null; void renderRecords(); }
    await renderRecipes();
  }, 'danger'));
  const schedule = r.scheduleMinutes ? `Every ${r.scheduleMinutes >= 1440 ? plural(r.scheduleMinutes / 1440, 'day') : r.scheduleMinutes >= 60 ? plural(r.scheduleMinutes / 60, 'hour') : `${r.scheduleMinutes} min`}` : 'Manual';
  return h('tr', {},
    h('td', {}, h('div', { className: 'recipe-name' }, r.name), h('div', { className: 'small' }, new URL(r.url).host),
      entry.needsAttention ? h('span', { className: 'badge status-needs_attention' }, 'Needs attention: schedules paused') : null,
      siteAllowed ? null : h('span', { className: 'badge pending' }, 'Site not allowed yet')),
    h('td', {}, schedule),
    h('td', {}, r.useNativeHost ? 'Private browser' : 'Background tab'),
    h('td', {}, last ? h('div', {}, statusBadge(last.status), ' ', h('span', { className: 'small', title: when(last.startedAt) }, ago(last.startedAt))) : h('span', { className: 'small' }, 'Never')),
    h('td', {}, records),
    h('td', {}, actions));
}

const plural = (n: number, unit: string) => (n === 1 ? `${unit}` : `${n} ${unit}s`);

async function renderRuns() {
  const recipes = await store.getRecipes();
  const runs: RunRecord[] = await store.listRuns(50);
  $('runs').replaceChildren(
    h('tr', {}, ...['Started', 'Recipe', 'Via', 'Status', 'Rows', 'Details'].map(t => h('th', {}, t))),
    ...(runs.length ? runs.map(run => h('tr', {},
      h('td', { title: when(run.startedAt) }, ago(run.startedAt)),
      h('td', {}, recipes[run.recipeId]?.recipe.name ?? '(deleted)'),
      h('td', {}, `${run.via === 'host' ? 'Private browser' : run.via === 'runner' ? `Runner ${run.runner ?? ''}` : 'Tab'}, ${run.trigger}`),
      h('td', {}, statusBadge(run.status)),
      h('td', {}, run.rowCount ?? ''),
      h('td', {}, run.error ?? '', ...run.warnings.slice(0, 5).map(w => h('div', { className: 'small' }, w)), run.warnings.length > 5 ? h('div', { className: 'small' }, `…${run.warnings.length - 5} more`) : null)))
      : [h('tr', {}, h('td', { colSpan: 6, className: 'small' }, 'No runs yet.'))]));
}

// ── records ──────────────────────────────────────────────────────────────────

const PAGE = 100;
/** The open recipe's records, kept in memory while unlocked so search, sort and
 * paging don't decrypt again. Cleared on lock. */
let view: { recipe: Recipe; records: PlainRecord[]; duplicates: number; unreadable: number; query: string; sort: { key: string; dir: 1 | -1 }; page: number } | null = null;

/** Opens a recipe's records (newest first), removing repeat copies from later runs.
 * Only while unlocked. */
async function openRecords(recipeId: string): Promise<{ records: PlainRecord[]; duplicates: string[]; unreadable: number }> {
  if (!vault) throw new Error('The vault is locked.');
  const stored = await store.listRecords(recipeId);
  let opened: [string, PlainRecord][] = [];
  let unreadable = 0;
  for (const s of stored) {
    try { opened.push([s.key, JSON.parse(vault.open(recipeId, s.key, s.sealed)) as PlainRecord]); }
    catch { unreadable++; }
  }
  // Runs store everything they collect (keys are random, so storage can't tell
  // duplicates apart). Now that the records are open, drop repeat copies.
  const duplicates = JSON.parse(core.duplicateKeys(JSON.stringify(opened))) as string[];
  if (duplicates.length) {
    await store.deleteRecordKeys(recipeId, duplicates);
    const gone = new Set(duplicates);
    opened = opened.filter(([key]) => !gone.has(key));
  }
  const records = opened.map(([, r]) => r);
  records.sort((a, b) => b.observedAt.localeCompare(a.observedAt));
  return { records, duplicates, unreadable };
}

function renderRecipePicker(recipes: Recipe[]) {
  const picker = $('recordsRecipe') as HTMLSelectElement;
  picker.replaceChildren(h('option', { value: '' }, recipes.length ? 'Choose a recipe…' : 'No recipes yet'),
    ...recipes.map(r => h('option', { value: r.id }, r.name)));
  picker.value = selected && recipes.some(r => r.id === selected) ? selected : '';
}

async function showRecords(recipeId: string, switchTab = true) {
  selected = recipeId;
  if (switchTab) showTab('records');
  ($('recordsRecipe') as HTMLSelectElement).value = recipeId;
  const entry = (await store.getRecipes())[recipeId];
  if (!entry) return;
  $('recordsTitle').textContent = `Records: ${entry.recipe.name}`;
  if (vault) {
    await touch();
    const { records, duplicates, unreadable } = await openRecords(recipeId);
    const keep = view?.recipe.id === recipeId ? view : null;
    view = { recipe: entry.recipe, records, duplicates: duplicates.length, unreadable, query: keep?.query ?? '', sort: keep?.sort ?? { key: 'observedAt', dir: -1 }, page: 0 };
  }
  await renderRecords();
}

/** The records matching the search, in the chosen order. */
function visible(): PlainRecord[] {
  if (!view) return [];
  const q = view.query.trim().toLowerCase();
  const matching = q ? view.records.filter(r => r.pageUrl.toLowerCase().includes(q) || Object.values(r.fields).some(v => v.toLowerCase().includes(q))) : view.records;
  const { key, dir } = view.sort;
  const value = (r: PlainRecord) => (key === 'observedAt' ? r.observedAt : key === 'pageUrl' ? r.pageUrl : r.fields[key] ?? '');
  const compare = (a: string, b: string) => a.localeCompare(b, undefined, { numeric: true, sensitivity: 'base' });
  // Rows from one run share a time: ties keep page order, then the first field.
  const first = view.recipe.fields[0]?.name ?? '';
  return [...matching].sort((a, b) => dir * compare(value(a), value(b)) || compare(a.pageUrl, b.pageUrl) || compare(a.fields[first] ?? '', b.fields[first] ?? ''));
}

async function renderRecords() {
  const box = $('records');
  if (!selected || !(await store.getRecipes())[selected]) {
    box.replaceChildren(empty('No recipe chosen', 'Choose a recipe above, or click Records on the Recipes tab.'));
    return;
  }
  if (!vault || !view) {
    box.replaceChildren(empty('The vault is locked', 'Unlock it above to read records.'));
    return;
  }
  const v = view;
  const names = v.recipe.fields.map(f => f.name);
  const search = h('input', { type: 'search', placeholder: 'Search these records', value: v.query, ariaLabel: 'Search records' });
  const tableBox = h('div', { className: 'scroll' });
  const pager = h('div', { className: 'pager' });
  const count = h('span', { className: 'small' });
  const exportAs = (format: 'csv' | 'json') => () => {
    const rows = visible();
    const name = `${fileName(v.recipe.name)}.${format}`;
    if (format === 'csv') download(name, 'text/csv', core.toCsv(JSON.stringify(names), JSON.stringify(rows)));
    else download(name, 'application/json', JSON.stringify(rows, null, 2));
  };
  const draw = () => {
    const rows = visible();
    const pages = Math.max(1, Math.ceil(rows.length / PAGE));
    v.page = Math.min(v.page, pages - 1);
    const shown = rows.slice(v.page * PAGE, (v.page + 1) * PAGE);
    count.textContent = v.query ? `${rows.length} of ${v.records.length} match. Exports include only these.` : '';
    const header = (key: string, label: string) => {
      const active = v.sort.key === key;
      return h('th', { ariaSort: active ? (v.sort.dir === 1 ? 'ascending' : 'descending') : 'none' },
        h('button', { className: 'sort', title: `Sort by ${label}`, onclick: () => { v.sort = { key, dir: active ? (v.sort.dir === 1 ? -1 : 1) : 1 }; v.page = 0; draw(); } }, label, active ? (v.sort.dir === 1 ? ' ▲' : ' ▼') : ''));
    };
    const kinds = new Map(v.recipe.fields.map(f => [f.name, f.kind]));
    tableBox.replaceChildren(shown.length ? h('table', {},
      h('tr', {}, header('observedAt', 'Observed'), ...names.map(n => header(n, n)), header('pageUrl', 'Page')),
      ...shown.map(r => h('tr', {},
        h('td', {}, when(r.observedAt)),
        ...names.map(n => h('td', {}, kinds.get(n) === 'href' ? link(r.fields[n] ?? '') : r.fields[n] ?? '')),
        h('td', {}, link(r.pageUrl, shortUrl(r.pageUrl))))))
      : empty(v.records.length ? 'Nothing matches' : 'No records yet', v.records.length ? 'Try a different search.' : 'Run the recipe to collect some.'));
    pager.replaceChildren(...(pages > 1 ? [
      h('button', { disabled: v.page === 0, onclick: () => { v.page--; draw(); } }, '‹ Newer'),
      h('span', { className: 'small' }, `Page ${v.page + 1} of ${pages}`),
      h('button', { disabled: v.page >= pages - 1, onclick: () => { v.page++; draw(); } }, 'Older ›')] : []));
  };
  let debounce: ReturnType<typeof setTimeout> | undefined;
  search.addEventListener('input', () => { clearTimeout(debounce); debounce = setTimeout(() => { v.query = search.value; v.page = 0; draw(); }, 150); });
  box.replaceChildren(
    h('p', { className: 'small' }, `${v.records.length} record(s)${v.duplicates ? `; ${v.duplicates} repeat copies from later runs removed` : ''}.`),
    ...(v.unreadable ? [h('p', { className: 'message bad' }, `${v.unreadable} record(s) could not be opened: they were modified or belong to another vault.`)] : []),
    h('div', { className: 'toolbar' }, search, h('button', { onclick: exportAs('csv') }, 'Export CSV'), h('button', { onclick: exportAs('json') }, 'Export JSON')),
    count, tableBox, pager);
  draw();
}

function shortUrl(url: string): string {
  try {
    const u = new URL(url);
    const path = u.pathname + u.search;
    return path.length > 40 ? `${path.slice(0, 39)}…` : path;
  } catch { return url; }
}

// ── requests from Claude ─────────────────────────────────────────────────────

/** Requests from Claude (via the service worker) that need the unlocked vault. They
 * don't count as activity: the vault still locks on its own schedule. */
async function vaultCommand(command: string, args: Record<string, unknown>): Promise<unknown> {
  const recipeId = String(args.recipeId ?? args.id ?? '');
  const entry = (await store.getRecipes())[recipeId];
  if (!entry) throw new Error('Unknown recipe');
  if (command === 'readRecords') {
    const { records, unreadable } = await openRecords(recipeId);
    const limit = Math.min(5000, Math.max(1, Number(args.limit) || 100));
    return { total: records.length, unreadable, records: records.slice(0, limit) };
  }
  if (command === 'exportRecords') {
    const { records } = await openRecords(recipeId);
    const text = args.format === 'csv' ? core.toCsv(JSON.stringify(entry.recipe.fields.map(f => f.name)), JSON.stringify(records)) : JSON.stringify(records, null, 2);
    return { text, count: records.length };
  }
  if (command === 'exportBundle') {
    // A bundle lets runners use this site's sign-ins: only for sites the person allowed.
    const origin = new URL(entry.recipe.url).origin;
    if (!(await chrome.permissions.contains({ origins: [origin + '/*'] }))) throw new Error(`Allow access to ${origin} in the dashboard first.`);
    const { runTimeoutMs } = await store.getSettings();
    const header = await store.getVaultHeader();
    return { bundle: vault!.exportBundle(JSON.stringify(entry.recipe), new Date().toISOString(), runTimeoutMs), fingerprint: header ? core.vaultFingerprint(header) : '' };
  }
  throw new Error(`Unknown vault command ${command}`);
}

chrome.runtime.onMessage.addListener((message, sender, sendResponse) => {
  // Only the extension's own service worker relays Claude's requests.
  if (message?.type !== 'vaultRequest' || sender.id !== chrome.runtime.id || sender.url !== chrome.runtime.getURL('background.js')) return false;
  // A locked dashboard stays silent, so an unlocked one (another tab) can answer.
  if (!vault) return false;
  if (message.command === 'vaultState') {
    sendResponse({ ok: true, result: { unlocked: true } });
    return false;
  }
  void vaultCommand(String(message.command), (message.args ?? {}) as Record<string, unknown>).then(
    result => sendResponse({ ok: true, result }),
    (error: unknown) => sendResponse({ ok: false, error: String((error as Error)?.message ?? error) }));
  return true;
});

// ── settings ─────────────────────────────────────────────────────────────────

async function renderSettings() {
  const header = await store.getVaultHeader();
  const box = $('vaultSettings');
  if (!header) box.replaceChildren(h('p', { className: 'small' }, 'Create the vault above first.'));
  else if (!vault) box.replaceChildren(...fingerprintLine(header), h('p', { className: 'small' }, 'Unlock the vault to change its passphrase.'));
  else {
    const oldPass = h('input', { type: 'password', autocomplete: 'current-password', placeholder: 'Current passphrase' });
    const newPass = h('input', { type: 'password', autocomplete: 'new-password', placeholder: 'New passphrase' });
    box.replaceChildren(...fingerprintLine(header),
      h('label', {}, 'Change passphrase'),
      h('div', { className: 'row' }, oldPass, newPass, h('button', { className: 'tight', onclick: async () => {
        try {
          const current = await store.getVaultHeader();
          if (!current) throw new Error('The vault is gone.');
          await store.setVaultHeader(core.changePassphrase(current, oldPass.value, newPass.value));
          oldPass.value = newPass.value = '';
          say('Passphrase changed.');
        } catch (e) { say((e as Error).message, true); }
      } }, 'Change')),
      h('p', { className: 'small' }, 'This re-wraps the vault key; records stay as they are.'));
  }

  const s = await store.getSettings();
  const lockMinutes = h('input', { type: 'number', min: '1', max: '240', value: String(s.autoLockMinutes) });
  const timeout = h('input', { type: 'number', min: '10', max: '3600', value: String(s.runTimeoutMs / 1000) });
  const saveRuns = async () => {
    try {
      await store.updateSettings({ autoLockMinutes: Number(lockMinutes.value), runTimeoutMs: Math.round(Number(timeout.value) * 1000) });
      say('Settings saved.');
      await touch();
      await renderVault();
    } catch (e) { say((e as Error).message.replace('autoLockMinutes', 'Auto-lock').replace('runTimeoutMs', 'Run time limit'), true); }
  };
  $('runSettings').replaceChildren(
    h('div', {}, h('label', {}, 'Lock the vault after (minutes idle)'), lockMinutes, h('p', { className: 'small' }, '1–240. Claude can shorten this but not lengthen it.')),
    h('div', {}, h('label', {}, 'Stop a run after (seconds)'), timeout, h('p', { className: 'small' }, '10–3600. Applies to runs here and to bundles exported afterwards.')));
  $('runSettingsSave').replaceChildren(h('button', { className: 'primary', onclick: saveRuns }, 'Save settings'));
}

async function renderClaude() {
  const { claudeAccess } = await store.getSettings();
  const toggle = h('input', { type: 'checkbox', checked: claudeAccess, onchange: async () => {
    await store.setClaudeAccess(toggle.checked);
    say(toggle.checked ? 'Claude access on: the MCP server can now manage Pocket Scraper Secure.' : 'Claude access off.');
  } });
  $('claude').replaceChildren(
    h('label', { className: 'check' }, toggle, 'Let Claude manage Pocket Scraper Secure (MCP)'),
    h('p', { className: 'small' }, 'Claude can then list, create and run recipes, manage runners, import results and change settings. It can read records only while this dashboard is unlocked. Deletes, new runners and new sites need your confirmation here.'),
    h('p', { className: 'small' }, host.installed ? 'Needs pocket-scraper-mcp registered with Claude (see docs/MCP.md).' : 'Needs the native host, which isn’t installed (see docs/MCP.md).'));
}

// ── runners and imports ──────────────────────────────────────────────────────

async function renderRunners() {
  const runners = await store.getRunners();
  const name = h('input', { placeholder: 'Name, e.g. build-server' });
  const key = h('input', { placeholder: 'Runner key (from pocket-scraper-runner init)' });
  const shown = h('p', { className: 'small' }, 'On the runner, run pocket-scraper-runner init and paste the key it prints.');
  key.addEventListener('input', () => {
    try { shown.textContent = `Fingerprint ${core.keyFingerprint(key.value)}: check it matches what the runner printed.`; }
    catch { shown.textContent = 'On the runner, run pocket-scraper-runner init and paste the key it prints.'; }
  });
  $('runners').replaceChildren(
    ...(runners.length ? runners.map(r => h('div', { className: 'runner' },
      h('div', { className: 'grow' }, h('span', { className: 'recipe-name' }, r.name), ' ', r.pending ? h('span', { className: 'badge pending' }, 'Waiting for approval') : null,
        h('div', { className: 'small' }, r.pending ? 'Added by Claude: compare the fingerprint with what the runner printed, then approve.' : `Added ${when(r.addedAt)}`)),
      h('code', { className: 'small' }, r.fingerprint),
      r.pending ? h('button', { className: 'tight primary', onclick: async () => {
        await store.setRunners((await store.getRunners()).map(x => (x.key === r.key ? { ...x, pending: false } : x)));
        say(`Approved ${r.name}.`);
      } }, 'Approve') : null,
      h('button', { className: 'tight danger', onclick: async () => {
        if (!confirm(`Remove runner “${r.name}”? Its future results will be refused.`)) return;
        await store.setRunners((await store.getRunners()).filter(x => x.key !== r.key));
        await renderRunners();
      } }, 'Remove')))
      : [empty('No runners yet', 'Add one below to run recipes on a server, a VM or on a schedule without Chrome.')]),
    h('h2', {}, 'Add a runner'),
    h('div', { className: 'row' }, name, key, h('button', { className: 'tight', onclick: async () => {
      try {
        const fingerprint = core.keyFingerprint(key.value);
        if (!name.value.trim()) throw new Error('Give the runner a name.');
        const list = await store.getRunners();
        if (list.some(x => x.key === key.value.trim())) throw new Error('That runner is already enrolled.');
        list.push({ name: name.value.trim().slice(0, 60), key: key.value.trim(), fingerprint, addedAt: new Date().toISOString() });
        await store.setRunners(list);
        say(`Enrolled ${name.value.trim()} (${fingerprint}).`);
        await renderRunners();
      } catch (e) { say((e as Error).message, true); }
    } }, 'Add runner')),
    shown);
}

function showImport(lines: string[]) {
  $('importMessage').replaceChildren(...lines.map(l => h('div', {}, l)));
}

/** Imports batch files and shows what happened. */
async function importBatches(files: BatchFile[], skipped: string[] = []): Promise<ImportOutcome> {
  const outcome = await importBatchFiles(core, files);
  showImport([...outcome.lines, ...skipped.map(key => `${key}: couldn't be fetched this time`)]);
  await renderRecipes();
  await renderRuns();
  if (selected && vault) await showRecords(selected, false);
  return outcome;
}

async function collect(retryDeferred = false) {
  $('importMessage').textContent = 'Collecting from the bucket…';
  const result = await collectBatches(retryDeferred);
  if (!result.ok) return say(result.message, true);
  // Too large ever to be a batch: refused for good.
  const oversized = result.skipped.filter(s => /larger than/.test(s.reason)).map(s => s.key);
  if (!result.files.length && !result.skipped.length) { $('importMessage').textContent = retryDeferred ? 'Nothing set aside.' : 'No new results in the bucket.'; return; }
  const outcome = await importBatches(result.files.map(f => ({ name: f.key, size: f.text.length, text: async () => f.text })), result.skipped.map(s => s.key));
  await ackBatches([...outcome.handled, ...oversized], outcome.deferred);
  if (result.more) $('importMessage').append(h('div', {}, 'More waiting: collect again.'));
}

function bindImports() {
  const picker = $('importBatches') as HTMLInputElement;
  picker.addEventListener('change', () => { if (picker.files?.length) void importBatches([...picker.files]).finally(() => { picker.value = ''; }); });
  const zone = $('dropzone');
  zone.addEventListener('dragover', e => { e.preventDefault(); zone.classList.add('over'); });
  zone.addEventListener('dragleave', () => zone.classList.remove('over'));
  zone.addEventListener('drop', e => {
    e.preventDefault();
    zone.classList.remove('over');
    const files = [...(e.dataTransfer?.files ?? [])];
    if (files.length) void importBatches(files);
  });
  ($('collect') as HTMLButtonElement).onclick = () => void collect().catch(e => say((e as Error).message, true));
  ($('retryDeferred') as HTMLButtonElement).onclick = () => void collect(true).catch(e => say((e as Error).message, true));
}

const TOUR: TourStep[] = [
  { target: '#vault', title: 'Your vault', body: 'Everything collected is encrypted with your passphrase. Unlock here to read records; it locks itself when you step away.' },
  { target: '#tab-recipes', title: 'Recipes', body: 'Recipes you made in the toolbar popup. Run them, see how their last run went, and set up sign-in for sites that need it.' },
  { target: '#tab-records', title: 'Records', body: 'What your recipes collected. Search, sort and export to CSV or JSON while the vault is unlocked.' },
  { target: '#tab-runners', title: 'Runners', body: 'Run recipes on a server, a VM or a timer without Chrome. Their results come back encrypted; import them here or collect them from a bucket.' },
  { target: '#tab-settings', title: 'Settings', body: 'Change your passphrase, how soon the vault locks, how long a run may take, and whether Claude may help manage things.' },
  { target: '#tour', title: 'Any time', body: 'Click Take the tour to see this again.' }
];

(async () => {
  core = await loadCore();
  $('tour').onclick = () => startTour('dashboard', TOUR);
  for (const event of ['pointerdown', 'keydown']) document.addEventListener(event, () => void touch());
  window.addEventListener('pagehide', () => vault?.free());
  for (const t of TABS) $(`tab-${t}`).onclick = () => showTab(t);
  showTab((TABS as readonly string[]).includes(location.hash.slice(1)) ? (location.hash.slice(1) as Tab) : 'recipes');
  ($('recordsRecipe') as HTMLSelectElement).onchange = e => {
    const id = (e.target as HTMLSelectElement).value;
    if (id) void showRecords(id, false).catch(err => say((err as Error).message, true));
    else { selected = null; view = null; void renderRecords(); }
  };
  await renderVault();
  await renderRecipes();
  await renderClaude();
  await renderSettings();
  await renderRunners();
  await renderRuns();
  await renderRecords();
  bindImports();
  chrome.storage.onChanged.addListener(changes => {
    if (changes.recipes) void renderRecipes();
    if (changes.runners) void renderRunners();
    if (changes.claudeAccess) void renderClaude();
    if (changes.settings) { void renderSettings(); void renderVault(); }
    if (changes.vault && !vault) { void renderVault(); void renderSettings(); }
  });
  setInterval(() => { if (document.visibilityState === 'visible') void renderRuns(); }, 3_000);
  await tourOnce('dashboard', TOUR);
})().catch(e => say((e as Error).message, true));
