// Dashboard: the only place records are decrypted. The unlocked key lives in this
// page's WebAssembly memory and is freed (zeroized) on lock, on inactivity and when
// the page closes.
import type { UnlockedVault } from '../build/core/pocket_scraper_core_wasm.js';
import { loadCore, type Core } from './core.js';
import { $, download, h, send } from './dom.js';
import { ackBatches, collectBatches, decideBucket, pendingBuckets } from './host.js';
import { importBatchFiles, type BatchFile, type ImportOutcome } from './importer.js';
import * as store from './store.js';
import { startTour, tourOnce, type TourStep } from './tour.js';
import type { PlainRecord, Recipe, RecipeEntry, RunRecord } from './types.js';

let core: Core;
let vault: UnlockedVault | null = null;
let lockTimer: ReturnType<typeof setTimeout> | undefined;
/** When the vault locks itself (ms since the epoch). */
let lockAt = 0;
/** Counts locks, so work that awaited across a lock drops what it opened. */
let lockEpoch = 0;
let selected: string | null = null;
let host: { installed: boolean; version?: string } = { installed: false };

const DOCS = 'https://github.com/ninjadogg/pocket-scraper-secure/blob/main/docs/';

/** Shows a message in a section's status line (the vault's, by default). */
function say(text: string, bad = false, where = 'message') {
  const m = $(where);
  m.textContent = text;
  m.classList.toggle('bad', bad);
}

// ── tabs ─────────────────────────────────────────────────────────────────────

const TABS = ['recipes', 'records', 'runners', 'settings'] as const;
type Tab = (typeof TABS)[number];

function showTab(tab: Tab, focus = false) {
  for (const t of TABS) {
    const button = $(`tab-${t}`);
    button.setAttribute('aria-selected', String(t === tab));
    button.tabIndex = t === tab ? 0 : -1;
    $(`panel-${t}`).hidden = t !== tab;
  }
  if (focus) $(`tab-${tab}`).focus();
  history.replaceState(null, '', `#${tab}`);
  // Claude's bucket proposals arrive through the helper app: look when Settings opens.
  if (tab === 'settings' && core) void renderBuckets();
}

/** Arrow keys, Home and End move between tabs (the WAI-ARIA tabs pattern). */
function onTabKey(e: KeyboardEvent) {
  const current = TABS.findIndex(t => $(`tab-${t}`).getAttribute('aria-selected') === 'true');
  const next = { ArrowRight: current + 1, ArrowLeft: current - 1, Home: 0, End: TABS.length - 1 }[e.key];
  if (next === undefined) return;
  e.preventDefault();
  showTab(TABS[(next + TABS.length) % TABS.length], true);
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

const STATUS: Record<string, string> = { success: 'Success', error: 'Error', needs_attention: 'Needs attention', running: 'Running', interrupted: 'Interrupted' };
const statusBadge = (status: string) => h('span', { className: `badge status-${status}` }, STATUS[status] ?? status.replace('_', ' '));

/** A link only for http(s) addresses; anything else (scraped, so untrusted) stays text. */
function link(url: string, text = url): Node {
  return /^https?:\/\//i.test(url) ? h('a', { href: url, target: '_blank', rel: 'noopener noreferrer', title: url }, text) : document.createTextNode(url);
}

function empty(title: string, text: string): HTMLElement {
  return h('div', { className: 'empty' }, h('strong', {}, title), text);
}

const plural = (n: number, one: string, many = `${one}s`) => `${n} ${n === 1 ? one : many}`;
const fileName = (name: string) => name.replace(/[^\w.-]+/g, '_').slice(0, 60) || 'records';
/** Lets the browser paint (a "working" state) before slow synchronous work. */
const paint = () => new Promise(r => setTimeout(r, 30));

/** Re-renders without losing keyboard focus: focus goes back to the element with the
 * same id, if it still exists. */
async function keepFocus(update: () => Promise<void> | void) {
  const id = document.activeElement?.id;
  await update();
  if (id && document.activeElement?.id !== id) document.getElementById(id)?.focus();
}

// ── vault ────────────────────────────────────────────────────────────────────

function lock() {
  vault?.free();
  vault = null;
  view = null;
  lockEpoch++;
  clearTimeout(lockTimer);
  void renderVault();
  void renderRecipes();
  void renderRecords();
  void renderSettings();
}

function armLock() {
  clearTimeout(lockTimer);
  if (vault) lockTimer = setTimeout(lock, Math.max(0, lockAt - Date.now()));
}

/** The person did something: the auto-lock starts again. */
async function touch() {
  if (!vault) return;
  const { autoLockMinutes } = await store.getSettings();
  lockAt = Date.now() + autoLockMinutes * 60_000;
  armLock();
}

/** Settings changed (maybe in another tab, or by Claude): a shorter auto-lock applies
 * at once; a longer one only from the person's next activity. */
async function settingsChanged() {
  if (vault) {
    const { autoLockMinutes } = await store.getSettings();
    lockAt = Math.min(lockAt, Date.now() + autoLockMinutes * 60_000);
    armLock();
  }
  await renderSettings();
  await renderVault();
}

function labelled(text: string, input: HTMLInputElement): HTMLElement {
  return h('label', { className: 'field' }, h('span', {}, text), input);
}

async function renderVault() {
  const header = await store.getVaultHeader();
  const box = $('vault');
  if (!header) {
    const pass = h('input', { type: 'password', autocomplete: 'new-password', placeholder: `At least ${core.minPassphraseChars()} characters` });
    const again = h('input', { type: 'password', autocomplete: 'new-password' });
    const create = h('button', { className: 'primary tight', onclick: async () => {
      if (pass.value !== again.value) return say('The passphrases don’t match.', true);
      create.disabled = true;
      try {
        say('Creating the vault…');
        await paint();
        await store.setVaultHeader(core.createVault(pass.value));
        pass.value = again.value = '';
        say('Vault created. Unlock it to read records.');
        await renderVault();
      } catch (e) { say((e as Error).message, true); }
      finally { create.disabled = false; }
    } }, 'Create');
    again.addEventListener('keydown', e => { if (e.key === 'Enter') create.click(); });
    box.replaceChildren(
      h('h2', { className: 'card-title' }, 'Create your vault'),
      h('p', { className: 'small' }, 'Everything you collect is encrypted with this passphrase. There is no recovery: if you forget it, stored records are lost. Keep it in your password manager.'),
      h('div', { className: 'row' }, labelled('Passphrase', pass), labelled('Repeat passphrase', again), create));
    return;
  }
  if (!vault) {
    const pass = h('input', { type: 'password', autocomplete: 'current-password' });
    const button = h('button', { className: 'primary tight' }, 'Unlock');
    const unlock = async () => {
      button.disabled = true;
      try {
        say('Unlocking…');
        await paint();
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
      finally { button.disabled = false; }
    };
    button.onclick = unlock;
    pass.addEventListener('keydown', e => { if (e.key === 'Enter') void unlock(); });
    box.replaceChildren(h('h2', { className: 'card-title' }, 'Vault locked'), h('div', { className: 'row' }, labelled('Passphrase', pass), button));
    return;
  }
  const { autoLockMinutes } = await store.getSettings();
  box.replaceChildren(h('div', { className: 'vault-bar' },
    h('strong', { className: 'status-success' }, 'Vault unlocked'),
    h('span', { className: 'grow small' }, `Locks after ${plural(autoLockMinutes, 'minute')} without activity.`),
    h('button', { className: 'tight', onclick: lock }, 'Lock now')));
}

// ── recipes and runs ─────────────────────────────────────────────────────────

/** Recipes whose review panel is open. */
const reviewing = new Set<string>();

async function renderRecipes() {
  const recipes = await store.getRecipes();
  host = await send<{ installed: boolean; version?: string }>({ type: 'hostStatus' }).catch(() => ({ installed: false }));
  $('host').replaceChildren(...(host.installed
    ? [`The helper app ${host.version} is installed: recipes can run in their own browser profile, even while Chrome is closed.`]
    : ['Runs happen in a background tab in this browser. Optional: install the helper app to run recipes in their own browser profile, even while Chrome is closed (', link('https://github.com/ninjadogg/pocket-scraper-secure#install', 'how to install it'), ').']));
  $('collect').hidden = !host.installed;
  $('retryDeferred').hidden = !host.installed;
  const lastRun = new Map<string, RunRecord>();
  for (const run of await store.listRuns(500)) if (!lastRun.has(run.recipeId)) lastRun.set(run.recipeId, run);
  const sorted = Object.values(recipes).sort((a, b) => a.recipe.name.localeCompare(b.recipe.name));
  const rows = (await Promise.all(sorted.map(async e =>
    recipeRows(e, await store.countRecords(e.recipe.id), lastRun.get(e.recipe.id), await chrome.permissions.contains({ origins: [new URL(e.recipe.url).origin + '/*'] }))))).flat();
  await keepFocus(() => {
    $('recipes').replaceChildren(
      h('tr', {}, ...['Recipe', 'Schedule', 'Runs on', 'Last run', 'Records', ''].map(t => h('th', {}, t))),
      ...(rows.length ? rows : [h('tr', {}, h('td', { colSpan: 6 }, empty('No recipes yet', 'Open the site you want to collect from and click the Pocket Scraper Secure button in Chrome’s toolbar. If you don’t see it, pin it from the puzzle-piece Extensions menu.')))]));
  });
  renderRecipePicker(sorted.map(e => e.recipe));
}

function describeSchedule(minutes: number): string {
  if (!minutes) return 'Manual';
  if (minutes >= 1440) return minutes === 1440 ? 'Every day' : minutes === 10080 ? 'Every week' : `Every ${minutes / 1440} days`;
  if (minutes >= 60) return minutes === 60 ? 'Every hour' : `Every ${minutes / 60} hours`;
  return `Every ${minutes} min`;
}

function recipeRows(entry: RecipeEntry, records: number, last: RunRecord | undefined, siteAllowed: boolean): HTMLElement[] {
  const r = entry.recipe;
  const msg = (text: string, bad = false) => say(text, bad, 'recipesMessage');
  const act = (label: string, key: string, fn: () => Promise<unknown>, extra: { className?: string; aria?: string } = {}) => h('button', {
    id: `${key}-${r.id}`, className: extra.className ?? '', ariaLabel: extra.aria ?? `${label}: ${r.name}`,
    onclick: async () => { try { await fn(); } catch (e) { msg((e as Error).message, true); } }
  }, label);
  const actions = h('div', { className: 'actions' });
  if (entry.review) {
    actions.append(act('Review', 'review', async () => { reviewing.has(r.id) ? reviewing.delete(r.id) : reviewing.add(r.id); await renderRecipes(); }, { className: 'primary', aria: `Review the changes Claude made to ${r.name}` }));
  } else {
    actions.append(act('Run now', 'run', async () => { await send({ type: 'runNow', id: r.id }); msg(`Running ${r.name}…`); }, { aria: `Run ${r.name} now` }));
  }
  actions.append(act('Records', 'records', () => showRecords(r.id), { aria: `Records of ${r.name}` }));
  if (!siteAllowed) {
    // Saved by Claude (which can't grant permissions): one click from the person does.
    const origin = new URL(r.url).origin;
    actions.prepend(act('Allow access', 'allow', async () => {
      if (await chrome.permissions.request({ origins: [origin + '/*'] })) { msg(`Access to ${origin} allowed.`); await renderRecipes(); }
    }, { className: 'primary', aria: `Allow access to ${origin} for ${r.name}` }));
  }
  if (vault && !entry.review) {
    actions.append(act('Export for runner', 'export', async () => {
      const { runTimeoutMs } = await store.getSettings();
      const header = await store.getVaultHeader();
      const file = `${fileName(r.name)}.pssbundle`;
      download(file, 'application/json', vault!.exportBundle(JSON.stringify(r), new Date().toISOString(), runTimeoutMs));
      showCommand(`Exported ${file}. Copy it to the runner, then run:`, `pocket-scraper-runner add ${file} --trust ${header ? core.vaultFingerprint(header) : ''}`);
    }, { aria: `Export ${r.name} for a runner` }));
  }
  if (entry.needsAttention) {
    if (r.useNativeHost && host.installed) {
      actions.append(act('Open session', 'open', async () => { await send({ type: 'session', id: r.id, action: 'open' }); msg('Sign in or complete verification in the window that opened, then click Finish session.'); }),
        act('Finish session', 'finish', async () => { await send({ type: 'session', id: r.id, action: 'close' }); msg('Session saved; schedules resume.'); await renderRecipes(); }));
    } else {
      actions.append(act('I’ve signed in', 'signedin', async () => { await send({ type: 'clearAttention', id: r.id }); await renderRecipes(); }));
    }
  } else if (r.useNativeHost && host.installed) {
    actions.append(act('Session setup', 'session', async () => { await send({ type: 'session', id: r.id, action: 'open' }); msg('Sign in in the window that opened, then click Finish session.'); await renderRecipes(); }),
      act('Finish session', 'finish', async () => { await send({ type: 'session', id: r.id, action: 'close' }); msg('Session saved.'); }));
  }
  actions.append(act('Delete', 'delete', async () => {
    if (!confirm(`Delete “${r.name}” and its ${plural(records, 'record')}? This can’t be undone.`)) return;
    await send({ type: 'deleteRecipe', id: r.id, purgeRecords: true });
    if (selected === r.id) { selected = null; view = null; void renderRecords(); }
    msg(`Deleted ${r.name}.`);
    await renderRecipes();
  }, { className: 'danger', aria: `Delete ${r.name}` }));
  const row = h('tr', {},
    h('td', {}, h('div', { className: 'recipe-name' }, r.name), h('div', { className: 'small' }, new URL(r.url).host),
      entry.review ? h('span', { className: 'badge pending' }, 'Changed by Claude: review before it runs') : null,
      entry.needsAttention ? h('span', { className: 'badge status-needs_attention' }, 'Needs attention: schedules paused') : null,
      siteAllowed ? null : h('span', { className: 'badge pending' }, 'Site not allowed yet')),
    h('td', {}, describeSchedule(r.scheduleMinutes)),
    h('td', {}, r.useNativeHost ? 'Helper app' : 'Background tab'),
    h('td', {}, last ? h('div', {}, statusBadge(last.status), ' ', h('span', { className: 'small', title: when(last.startedAt) }, ago(last.startedAt))) : h('span', { className: 'small' }, 'Never')),
    h('td', {}, records),
    h('td', {}, actions));
  return entry.review && reviewing.has(r.id) ? [row, reviewRow(entry)] : [row];
}

/** What a Claude-made recipe will do, for the person to approve. */
function reviewRow(entry: RecipeEntry): HTMLElement {
  const r = entry.recipe;
  const item = (label: string, ...value: (Node | string)[]) => h('div', { className: 'review-item' }, h('span', { className: 'small' }, label), h('div', {}, ...value));
  const steps = r.workflow
    ? r.workflow.steps.map((s, i) => `${i + 1}. ${s.type === 'click' ? `Click ${s.selector}` : s.type === 'wait' ? `Wait ${s.waitMs} ms` : 'Collect the fields'}`).join(' · ') + (r.workflow.repeatCount > 1 ? ` (repeated ${r.workflow.repeatCount} times)` : '')
    : [...r.actions.map(a => `Click ${a.selector}`), r.nextSelector ? `Then click ${r.nextSelector} for up to ${plural(r.maxPages, 'page')}` : ''].filter(Boolean).join(' · ') || 'No clicks: collects the first page only.';
  return h('tr', { className: 'review-row' }, h('td', { colSpan: 6 },
    h('p', {}, h('strong', {}, 'Claude created or changed this recipe.'), ' Check where it goes and what it clicks: it runs with ', r.useNativeHost ? 'the helper app’s own browser profile for this site.' : 'this browser, so it is signed in wherever you are.'),
    item('Starts at', link(r.url)),
    item('Clicks', steps),
    item('Collects', r.fields.map(f => f.name).join(', ')),
    item('Schedule', describeSchedule(r.scheduleMinutes)),
    h('div', { className: 'actions' },
      h('button', { id: `approve-${r.id}`, className: 'primary', onclick: async () => {
        await send({ type: 'approveRecipe', id: r.id });
        reviewing.delete(r.id);
        say(`Approved ${r.name}. It can run now.`, false, 'recipesMessage');
        await renderRecipes();
      } }, 'Approve'),
      h('button', { onclick: async () => { reviewing.delete(r.id); await renderRecipes(); } }, 'Not now'))));
}

/** A command the person needs to copy, kept until dismissed. */
function showCommand(text: string, command: string) {
  const box = $('recipesMessage');
  box.classList.remove('bad');
  const copy = h('button', { className: 'tight', onclick: async () => { await navigator.clipboard.writeText(command); copy.textContent = 'Copied'; } }, 'Copy');
  box.replaceChildren(h('div', { className: 'command' }, h('div', {}, text), h('div', { className: 'row' }, h('code', {}, command), copy, h('button', { className: 'tight', onclick: () => box.replaceChildren() }, 'Dismiss'))));
}

let runsShown = '';

async function renderRuns() {
  const recipes = await store.getRecipes();
  const runs: RunRecord[] = await store.listRuns(50);
  // Redrawn only when something changed, so keyboard and screen-reader positions stay.
  const signature = JSON.stringify(runs.map(r => [r.id, r.status, r.rowCount, r.error, r.warnings.length, recipes[r.recipeId]?.recipe.name]));
  if (signature === runsShown) return;
  runsShown = signature;
  const via = (run: RunRecord) => (run.via === 'host' ? 'Helper app' : run.via === 'runner' ? `Runner ${run.runner ?? ''}` : 'Background tab');
  $('runs').replaceChildren(
    h('tr', {}, ...['Started', 'Recipe', 'Ran on', 'Status', 'Rows', 'Details'].map(t => h('th', {}, t))),
    ...(runs.length ? runs.map(run => h('tr', {},
      h('td', { title: when(run.startedAt) }, ago(run.startedAt)),
      h('td', {}, recipes[run.recipeId]?.recipe.name ?? '(deleted)'),
      h('td', {}, `${via(run)} · ${run.trigger}`),
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
 * Only while unlocked; throws if the vault locks meanwhile. */
async function openRecords(recipeId: string): Promise<{ records: PlainRecord[]; duplicates: string[]; unreadable: number }> {
  if (!vault) throw new Error('The vault is locked.');
  const epoch = lockEpoch;
  const stored = await store.listRecords(recipeId);
  if (!vault || epoch !== lockEpoch) throw new Error('The vault is locked.');
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
  if (!vault || epoch !== lockEpoch) throw new Error('The vault is locked.');
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
    try {
      const { records, duplicates, unreadable } = await openRecords(recipeId);
      const keep = view?.recipe.id === recipeId ? view : null;
      view = { recipe: entry.recipe, records, duplicates: duplicates.length, unreadable, query: keep?.query ?? '', sort: keep?.sort ?? { key: 'observedAt', dir: -1 }, page: 0 };
    } catch { view = null; }
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
  const count = h('span', { className: 'small', role: 'status' });
  const exportAs = (format: 'csv' | 'json') => () => {
    const rows = visible();
    const name = `${fileName(v.recipe.name)}.${format}`;
    if (format === 'csv') download(name, 'text/csv', core.toCsv(JSON.stringify(names), JSON.stringify(rows)));
    else download(name, 'application/json', JSON.stringify(rows, null, 2));
  };
  const csv = h('button', { onclick: exportAs('csv') }, 'Export CSV');
  const json = h('button', { onclick: exportAs('json') }, 'Export JSON');
  const draw = () => {
    const rows = visible();
    const pages = Math.max(1, Math.ceil(rows.length / PAGE));
    v.page = Math.min(v.page, pages - 1);
    const shown = rows.slice(v.page * PAGE, (v.page + 1) * PAGE);
    csv.disabled = json.disabled = rows.length === 0;
    count.textContent = v.query ? `${rows.length} of ${v.records.length} match. Exports include only these.` : '';
    const header = (key: string, label: string) => {
      const active = v.sort.key === key;
      return h('th', { ariaSort: active ? (v.sort.dir === 1 ? 'ascending' : 'descending') : 'none' },
        h('button', { className: 'sort', title: `Sort by ${label}`, onclick: () => { v.sort = { key, dir: active ? (v.sort.dir === 1 ? -1 : 1) : 1 }; v.page = 0; draw(); } },
          label, active ? h('span', { ariaHidden: 'true' }, v.sort.dir === 1 ? ' ▲' : ' ▼') : null));
    };
    const kinds = new Map(v.recipe.fields.map(f => [f.name, f.kind]));
    tableBox.replaceChildren(shown.length ? h('table', {},
      h('tr', {}, header('observedAt', 'Observed'), ...names.map(n => header(n, n)), header('pageUrl', 'Page')),
      ...shown.map(r => h('tr', {},
        h('td', {}, when(r.observedAt)),
        ...names.map(n => h('td', {}, kinds.get(n) === 'href' ? link(r.fields[n] ?? '') : r.fields[n] ?? '')),
        h('td', {}, link(r.pageUrl, r.pageUrl === v.recipe.url ? 'Start page' : shortUrl(r.pageUrl))))))
      : empty(v.records.length ? 'Nothing matches' : 'No records yet', v.records.length ? 'Try a different search.' : 'Run the recipe to collect some.'));
    pager.replaceChildren(...(pages > 1 ? [
      h('button', { disabled: v.page === 0, onclick: () => { v.page--; draw(); } }, '‹ Previous'),
      h('span', { className: 'small' }, `Page ${v.page + 1} of ${pages}`),
      h('button', { disabled: v.page >= pages - 1, onclick: () => { v.page++; draw(); } }, 'Next ›')] : []));
  };
  let debounce: ReturnType<typeof setTimeout> | undefined;
  search.addEventListener('input', () => { clearTimeout(debounce); debounce = setTimeout(() => { v.query = search.value; v.page = 0; draw(); }, 150); });
  box.replaceChildren(
    h('p', { className: 'small' }, `${plural(v.records.length, 'record')}${v.duplicates ? `; ${plural(v.duplicates, 'repeat copy', 'repeat copies')} from later runs removed` : ''}.`),
    ...(v.unreadable ? [h('p', { className: 'message bad' }, `${plural(v.unreadable, 'record')} could not be opened: they were modified or belong to another vault.`)] : []),
    h('div', { className: 'toolbar' }, search, csv, json),
    h('p', { className: 'small' }, 'Exported files are not encrypted.'),
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
    if (entry.review) throw new Error('The person must review and approve this recipe in the dashboard first.');
    // A bundle lets runners use this site's sign-ins: only for sites the person allowed.
    const origin = new URL(entry.recipe.url).origin;
    if (!(await chrome.permissions.contains({ origins: [origin + '/*'] }))) throw new Error(`Allow access to ${origin} in the dashboard first.`);
    const { runTimeoutMs } = await store.getSettings();
    const header = await store.getVaultHeader();
    if (!vault) throw new Error('The vault is locked.');
    return { bundle: vault.exportBundle(JSON.stringify(entry.recipe), new Date().toISOString(), runTimeoutMs), fingerprint: header ? core.vaultFingerprint(header) : '' };
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
  const msg = (text: string, bad = false) => say(text, bad, 'settingsMessage');
  const header = await store.getVaultHeader();
  const box = $('vaultSettings');
  const fingerprint = header && core.vaultFingerprint(header);
  const fpLine = fingerprint ? [h('p', { className: 'small' }, 'Vault fingerprint (runners check it): ', h('code', {}, fingerprint))] : [];
  if (!header) box.replaceChildren(h('p', { className: 'small' }, 'Create the vault above first.'));
  else if (!vault) box.replaceChildren(...fpLine, h('p', { className: 'small' }, 'Unlock the vault to change its passphrase.'));
  else {
    const oldPass = h('input', { type: 'password', autocomplete: 'current-password' });
    const newPass = h('input', { type: 'password', autocomplete: 'new-password', placeholder: `At least ${core.minPassphraseChars()} characters` });
    const again = h('input', { type: 'password', autocomplete: 'new-password' });
    const change = h('button', { className: 'tight', onclick: async () => {
      if (newPass.value !== again.value) return msg('The new passphrases don’t match.', true);
      change.disabled = true;
      try {
        msg('Changing the passphrase…');
        await paint();
        const current = await store.getVaultHeader();
        if (!current) throw new Error('The vault is gone.');
        await store.setVaultHeader(core.changePassphrase(current, oldPass.value, newPass.value));
        oldPass.value = newPass.value = again.value = '';
        msg('Passphrase changed.');
      } catch (e) { msg((e as Error).message, true); }
      finally { change.disabled = false; }
    } }, 'Change');
    box.replaceChildren(...fpLine,
      h('h3', {}, 'Change passphrase'),
      h('div', { className: 'row' }, labelled('Current passphrase', oldPass), labelled('New passphrase', newPass), labelled('Repeat new passphrase', again), change),
      h('p', { className: 'small' }, 'Your records stay as they are. Only the passphrase that opens them changes. There is still no recovery.'));
  }

  const s = await store.getSettings();
  const lockMinutes = h('input', { id: 'autoLockMinutes', type: 'number', min: '1', max: '240', value: String(s.autoLockMinutes) });
  const timeout = h('input', { id: 'runTimeoutSeconds', type: 'number', min: '10', max: '3600', value: String(s.runTimeoutMs / 1000) });
  const saveRuns = async () => {
    try {
      await store.updateSettings({ autoLockMinutes: Number(lockMinutes.value), runTimeoutMs: Math.round(Number(timeout.value) * 1000) });
      msg('Settings saved.');
      await touch();
      await renderVault();
    } catch (e) { msg((e as Error).message.replace('autoLockMinutes', 'The auto-lock').replace('runTimeoutMs', 'The run time limit'), true); }
  };
  $('runSettings').replaceChildren(
    h('div', {}, h('label', { htmlFor: 'autoLockMinutes' }, 'Lock the vault after (minutes without activity)'), lockMinutes, h('p', { className: 'small' }, '1–240. Claude can shorten this but not lengthen it.')),
    h('div', {}, h('label', { htmlFor: 'runTimeoutSeconds' }, 'Stop a run after (seconds)'), timeout, h('p', { className: 'small' }, '10–3600. Applies to runs here and to bundles exported afterwards.')));
  $('runSettingsSave').replaceChildren(h('button', { className: 'primary', onclick: saveRuns }, 'Save settings'));
  await renderBuckets();
}

/** Bucket settings Claude proposed: they take effect only once approved here. */
async function renderBuckets() {
  const box = $('buckets');
  if (!host.installed) { box.replaceChildren(); return; }
  const items = await pendingBuckets().catch(() => []);
  if (!items.length) { box.replaceChildren(); return; }
  const role = (r: string) => (r === 'runner' ? 'Runner uploads (this computer’s runner)' : 'Collecting results (this computer)');
  box.replaceChildren(h('div', { className: 'card' },
    h('h3', {}, 'Bucket settings from Claude'),
    h('p', { className: 'small' }, 'Results will be sent to, or fetched from, this storage. Approve only if you recognise it.'),
    ...items.map(p => h('div', { className: 'runner' },
      h('div', { className: 'grow' }, h('strong', {}, role(p.role)), p.replaces ? h('span', { className: 'badge pending' }, 'Replaces current settings') : null,
        h('div', { className: 'small' }, `${p.endpoint} · bucket ${p.bucket}${p.prefix ? ` · folder ${p.prefix}` : ''} · ${p.region} · key ${p.accessKeyId}`)),
      h('button', { id: `bucket-approve-${p.role}`, className: 'tight primary', onclick: async () => {
        try { await decideBucket(p.role, true); say(`Bucket settings for ${p.role === 'runner' ? 'the runner' : 'collecting'} approved.`, false, 'settingsMessage'); } catch (e) { say((e as Error).message, true, 'settingsMessage'); }
        await renderBuckets();
      } }, 'Approve'),
      h('button', { className: 'tight danger', onclick: async () => {
        try { await decideBucket(p.role, false); say('Discarded.', false, 'settingsMessage'); } catch (e) { say((e as Error).message, true, 'settingsMessage'); }
        await renderBuckets();
      } }, 'Discard')))));
}

async function renderClaude() {
  const { claudeAccess } = await store.getSettings();
  const toggle = h('input', { type: 'checkbox', checked: claudeAccess, onchange: async () => {
    await store.setClaudeAccess(toggle.checked);
    say(!toggle.checked ? 'Claude access off.' : host.installed ? 'Claude access on: Claude can now manage Pocket Scraper Secure.' : 'Saved. Claude can connect once the helper app is installed.', false, 'settingsMessage');
  } });
  $('claude').replaceChildren(
    h('label', { className: 'check' }, toggle, 'Let Claude manage Pocket Scraper Secure (MCP)'),
    h('p', { className: 'small' }, 'Claude can then list, create and run recipes, manage runners, import results and propose bucket settings. It can read records only while this dashboard is unlocked. Recipes it creates or redirects, new runners, new sites and bucket settings wait for your approval here. Claude is told to ask you before deleting anything.'),
    h('p', { className: 'small' }, host.installed ? 'Needs pocket-scraper-mcp registered with Claude: ' : 'Needs the helper app, which isn’t installed: ', link(`${DOCS}MCP.md`, 'setup guide'), '.'));
}

// ── runners and imports ──────────────────────────────────────────────────────

/** Runners whose approval is being confirmed. */
const confirming = new Set<string>();

async function renderRunners() {
  const runners = await store.getRunners();
  const msg = (text: string, bad = false) => say(text, bad, 'runnersMessage');
  await keepFocus(() => $('runnerList').replaceChildren(
    ...(runners.length ? runners.map(r => h('div', { className: 'runner' },
      h('div', { className: 'grow' }, h('span', { className: 'recipe-name' }, r.name), ' ', r.pending ? h('span', { className: 'badge pending' }, 'Waiting for approval') : null,
        h('div', { className: 'small' }, r.pending ? 'Added by Claude. Approve only if the runner printed this fingerprint: ' : `Added ${when(r.addedAt)}. Fingerprint `, h('code', {}, r.fingerprint))),
      ...(r.pending ? (confirming.has(r.key)
        ? [h('span', { className: 'small' }, 'Same fingerprint as the runner printed?'),
          h('button', { id: `approve-yes-${r.fingerprint}`, className: 'tight primary', onclick: async () => {
            await store.setRunners((await store.getRunners()).map(x => (x.key === r.key ? { ...x, pending: false } : x)));
            confirming.delete(r.key);
            msg(`Approved ${r.name}.`);
          } }, 'Yes, approve'),
          h('button', { className: 'tight', onclick: async () => { confirming.delete(r.key); await renderRunners(); } }, 'Cancel')]
        : [h('button', { id: `approve-${r.fingerprint}`, className: 'tight primary', ariaLabel: `Approve runner ${r.name}`, onclick: async () => { confirming.add(r.key); await renderRunners(); document.getElementById(`approve-yes-${r.fingerprint}`)?.focus(); } }, 'Approve')]) : []),
      h('button', { id: `remove-${r.fingerprint}`, className: 'tight danger', ariaLabel: `Remove runner ${r.name}`, onclick: async () => {
        if (!confirm(`Remove runner “${r.name}”? Its future results will be refused.`)) return;
        await store.setRunners((await store.getRunners()).filter(x => x.key !== r.key));
        msg(`Removed ${r.name}.`);
      } }, 'Remove')))
      : [empty('No runners yet', 'Add one below to run recipes on a server, a VM or on a schedule without Chrome.')])));
}

/** The add-runner form, built once so a refresh (e.g. Claude adding a runner) never
 * wipes what the person is typing. */
function buildRunnerForm() {
  const msg = (text: string, bad = false) => say(text, bad, 'runnersMessage');
  const name = h('input', { id: 'runnerName' });
  const key = h('input', { id: 'runnerKey' });
  const hint = 'On the runner, run pocket-scraper-runner init and paste the key it prints.';
  const shown = h('p', { className: 'small' }, hint);
  key.addEventListener('input', () => {
    try { shown.textContent = `Fingerprint ${core.keyFingerprint(key.value)}: check it matches what the runner printed.`; }
    catch { shown.textContent = hint; }
  });
  $('runnerForm').replaceChildren(
    h('h2', {}, 'Add a runner'),
    h('div', { className: 'row' }, labelled('Name, e.g. build-server', name), labelled('Runner key', key), h('button', { className: 'tight', onclick: async () => {
      try {
        const fingerprint = core.keyFingerprint(key.value);
        if (!name.value.trim()) throw new Error('Give the runner a name.');
        const list = await store.getRunners();
        if (list.some(x => x.key === key.value.trim())) throw new Error('That runner is already enrolled.');
        list.push({ name: name.value.trim().slice(0, 60), key: key.value.trim(), fingerprint, addedAt: new Date().toISOString() });
        await store.setRunners(list);
        msg(`Enrolled ${name.value.trim()} (${fingerprint}).`);
        name.value = key.value = '';
        shown.textContent = hint;
      } catch (e) { msg((e as Error).message, true); }
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
  if (!result.ok) return say(result.message, true, 'importMessage');
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
  ($('collect') as HTMLButtonElement).onclick = () => void collect().catch(e => say((e as Error).message, true, 'importMessage'));
  ($('retryDeferred') as HTMLButtonElement).onclick = () => void collect(true).catch(e => say((e as Error).message, true, 'importMessage'));
}

// ── tour ─────────────────────────────────────────────────────────────────────

async function tourSteps(): Promise<TourStep[]> {
  const hasVault = Boolean(await store.getVaultHeader());
  return [
    hasVault
      ? { target: '#vault', title: 'Your vault', body: 'Everything you collect is encrypted with your passphrase. Unlock here to read records; it locks itself when you step away.' }
      : { target: '#vault', title: 'Start here: create your vault', body: 'Choose a passphrase to create your vault. Everything you collect is encrypted with it. It can’t be recovered, so keep it in your password manager.' },
    { target: '#tab-recipes', title: 'Recipes', body: 'Recipes you made in the toolbar popup. Run them, see how their last run went, and approve anything Claude set up.' },
    { target: '#tab-records', title: 'Records', body: 'What your recipes collected. Search, sort and export to CSV or JSON while the vault is unlocked.' },
    { target: '#tab-runners', title: 'Runners', body: 'Run recipes on a server, a VM or a timer without Chrome. Their results come back encrypted; import them here or collect them from a bucket.' },
    { target: '#tab-settings', title: 'Settings', body: 'Change your passphrase, how soon the vault locks, how long a run may take, the helper app, and whether Claude may help.' },
    { target: '#tour', title: 'Next: make a recipe', body: 'Open the site you want to collect from and click the Pocket Scraper Secure button in Chrome’s toolbar. If you don’t see it, pin it from the puzzle-piece Extensions menu. Click Take the tour to see this again.' }
  ];
}

(async () => {
  core = await loadCore();
  $('tour').onclick = async () => startTour('dashboard', await tourSteps());
  for (const event of ['pointerdown', 'keydown']) document.addEventListener(event, () => void touch());
  window.addEventListener('pagehide', () => vault?.free());
  for (const t of TABS) $(`tab-${t}`).onclick = () => showTab(t);
  document.querySelector('.tabs')?.addEventListener('keydown', e => onTabKey(e as KeyboardEvent));
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
  buildRunnerForm();
  await renderRunners();
  await renderRuns();
  await renderRecords();
  bindImports();
  chrome.storage.onChanged.addListener(changes => {
    if (changes.recipes) void renderRecipes();
    if (changes.runners) void renderRunners();
    if (changes.claudeAccess) void renderClaude();
    if (changes.settings) void settingsChanged();
    if (changes.vault && !vault) { void renderVault(); void renderSettings(); }
  });
  setInterval(() => { if (document.visibilityState === 'visible') void renderRuns(); }, 3_000);
  // Bucket proposals arrive from Claude through the helper app, not storage.
  setInterval(() => { if (document.visibilityState === 'visible' && !$('panel-settings').hidden) void renderBuckets(); }, 5_000);
  document.addEventListener('visibilitychange', () => { if (document.visibilityState === 'visible' && !$('panel-settings').hidden) void renderBuckets(); });
  await tourOnce('dashboard', await tourSteps());
})().catch(e => say((e as Error).message, true));
