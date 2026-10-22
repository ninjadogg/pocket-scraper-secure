// Dashboard: the only place records are decrypted. The unlocked key lives in this
// page's WebAssembly memory and is freed (zeroized) on lock, on inactivity and when
// the page closes.
import type { UnlockedVault } from '../build/core/pocket_scraper_core_wasm.js';
import { loadCore, type Core } from './core.js';
import { $, download, h, send } from './dom.js';
import * as store from './store.js';
import type { PlainRecord, RecipeEntry, RunRecord } from './types.js';

let core: Core;
let vault: UnlockedVault | null = null;
let lockTimer: ReturnType<typeof setTimeout> | undefined;
let selected: string | null = null;

function say(text: string, bad = false) {
  const m = $('message');
  m.textContent = text;
  m.classList.toggle('bad', bad);
}

function lock() {
  vault?.free();
  vault = null;
  clearTimeout(lockTimer);
  $('records').replaceChildren(h('p', { className: 'small' }, 'Unlock the vault to read records.'));
  void renderVault();
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
        vault = core.UnlockedVault.unlock(header, pass.value);
        pass.value = '';
        say('');
        await touch();
        await renderVault();
        if (selected) await showRecords(selected);
      } catch (e) { say((e as Error).message, true); }
    };
    pass.addEventListener('keydown', e => { if (e.key === 'Enter') void unlock(); });
    box.replaceChildren(h('strong', {}, 'Vault locked'), h('div', { className: 'row' }, pass, h('button', { className: 'primary tight', onclick: unlock }, 'Unlock')));
    return;
  }
  const oldPass = h('input', { type: 'password', autocomplete: 'current-password', placeholder: 'Current passphrase' });
  const newPass = h('input', { type: 'password', autocomplete: 'new-password', placeholder: 'New passphrase' });
  box.replaceChildren(
    h('div', { className: 'row' }, h('strong', {}, 'Vault unlocked'), h('button', { className: 'tight', onclick: lock }, 'Lock now')),
    h('details', {}, h('summary', {}, 'Change passphrase'), h('div', { className: 'row' }, oldPass, newPass, h('button', { className: 'tight', onclick: async () => {
      try {
        await store.setVaultHeader(core.changePassphrase(header, oldPass.value, newPass.value));
        oldPass.value = newPass.value = '';
        say('Passphrase changed.');
      } catch (e) { say((e as Error).message, true); }
    } }, 'Change'))));
}

async function renderRecipes() {
  const recipes = await store.getRecipes();
  const host = await send<{ installed: boolean; version?: string }>({ type: 'hostStatus' }).catch(() => ({ installed: false, version: undefined }));
  $('host').textContent = host.installed ? `Native host ${host.version} installed.` : 'Native host not installed: runs use a background tab in this browser.';
  const rows = await Promise.all(Object.values(recipes).sort((a, b) => a.recipe.name.localeCompare(b.recipe.name)).map(async e => recipeRow(e, await store.countRecords(e.recipe.id), host.installed)));
  $('recipes').replaceChildren(
    h('tr', {}, ...['Recipe', 'Site', 'Schedule', 'Runs in', 'Records', ''].map(t => h('th', {}, t))),
    ...(rows.length ? rows : [h('tr', {}, h('td', { colSpan: 6, className: 'small' }, 'No recipes yet. Use the toolbar popup on a website to create one.'))]));
}

function recipeRow(entry: RecipeEntry, records: number, hostInstalled: boolean): HTMLElement {
  const r = entry.recipe;
  const act = (label: string, fn: () => Promise<unknown>) => h('button', { onclick: async () => { try { await fn(); } catch (e) { say((e as Error).message, true); } } }, label);
  const actions = h('div', { className: 'actions' },
    act('Run now', async () => { await send({ type: 'runNow', id: r.id }); say(`Running ${r.name}…`); }),
    act('Records', () => showRecords(r.id)));
  if (entry.needsAttention) {
    if (r.useNativeHost && hostInstalled) {
      actions.append(act('Open session', async () => { await send({ type: 'session', id: r.id, action: 'open' }); say('Sign in or complete verification in the window that opened, then click Finish session.'); }),
        act('Finish session', async () => { await send({ type: 'session', id: r.id, action: 'close' }); say('Session saved; schedules resume.'); await renderRecipes(); }));
    } else {
      actions.append(act('I’ve signed in', async () => { await send({ type: 'clearAttention', id: r.id }); await renderRecipes(); }));
    }
  } else if (r.useNativeHost && hostInstalled) {
    actions.append(act('Session setup', async () => { await send({ type: 'session', id: r.id, action: 'open' }); say('Sign in in the window that opened, then click Finish session.'); await renderRecipes(); }),
      act('Finish session', async () => { await send({ type: 'session', id: r.id, action: 'close' }); say('Session saved.'); }));
  }
  actions.append(act('Delete', async () => {
    if (!confirm(`Delete “${r.name}”? Its encrypted records are deleted too.`)) return;
    await send({ type: 'deleteRecipe', id: r.id, purgeRecords: true });
    if (selected === r.id) { selected = null; $('records').replaceChildren(); }
    await renderRecipes();
  }));
  const schedule = r.scheduleMinutes ? `Every ${r.scheduleMinutes >= 1440 ? `${r.scheduleMinutes / 1440} day(s)` : r.scheduleMinutes >= 60 ? `${r.scheduleMinutes / 60} hour(s)` : `${r.scheduleMinutes} min`}` : 'Manual';
  return h('tr', {},
    h('td', {}, r.name, entry.needsAttention ? h('div', { className: 'status-needs_attention small' }, 'Needs attention: schedules paused') : null),
    h('td', {}, new URL(r.url).host),
    h('td', {}, schedule),
    h('td', {}, r.useNativeHost ? 'Native host' : 'Background tab'),
    h('td', {}, records),
    h('td', {}, actions));
}

async function showRecords(recipeId: string) {
  selected = recipeId;
  const entry = (await store.getRecipes())[recipeId];
  if (!entry) return;
  $('recordsTitle').textContent = `Records: ${entry.recipe.name}`;
  if (!vault) {
    $('records').replaceChildren(h('p', { className: 'small' }, 'Unlock the vault to read records.'));
    return;
  }
  await touch();
  const stored = await store.listRecords(recipeId);
  const opened: PlainRecord[] = [];
  let unreadable = 0;
  for (const s of stored) {
    try { opened.push(JSON.parse(vault.open(recipeId, s.key, s.sealed)) as PlainRecord); }
    catch { unreadable++; }
  }
  opened.sort((a, b) => b.observedAt.localeCompare(a.observedAt));
  const names = entry.recipe.fields.map(f => f.name);
  const shown = opened.slice(0, 500);
  const safeName = entry.recipe.name.replace(/[^\w.-]+/g, '_').slice(0, 60) || 'records';
  $('records').replaceChildren(
    h('div', { className: 'actions' },
      h('button', { onclick: () => download(`${safeName}.csv`, 'text/csv', core.toCsv(JSON.stringify(names), JSON.stringify(opened))) }, 'Export CSV'),
      h('button', { onclick: () => download(`${safeName}.json`, 'application/json', JSON.stringify(opened, null, 2)) }, 'Export JSON')),
    h('p', { className: 'small' }, `${opened.length} record(s)${opened.length > shown.length ? `, newest ${shown.length} shown` : ''}.`),
    ...(unreadable ? [h('p', { className: 'message bad' }, `${unreadable} record(s) could not be opened: they were modified or belong to another vault.`)] : []),
    h('div', { className: 'scroll' }, h('table', {},
      h('tr', {}, ...['Observed', ...names, 'Page'].map(t => h('th', {}, t))),
      ...shown.map(r => h('tr', {}, h('td', {}, new Date(r.observedAt).toLocaleString()), ...names.map(n => h('td', {}, r.fields[n] ?? '')), h('td', {}, r.pageUrl))))));
}

async function renderRuns() {
  const recipes = await store.getRecipes();
  const runs: RunRecord[] = await store.listRuns(50);
  $('runs').replaceChildren(
    h('tr', {}, ...['Started', 'Recipe', 'Via', 'Status', 'Rows (new)', 'Details'].map(t => h('th', {}, t))),
    ...runs.map(run => h('tr', {},
      h('td', {}, new Date(run.startedAt).toLocaleString()),
      h('td', {}, recipes[run.recipeId]?.recipe.name ?? '(deleted)'),
      h('td', {}, `${run.via === 'host' ? 'Native host' : 'Tab'}, ${run.trigger}`),
      h('td', { className: `status-${run.status}` }, run.status.replace('_', ' ')),
      h('td', {}, run.rowCount === undefined ? '' : `${run.rowCount} (${run.newCount ?? 0})`),
      h('td', {}, run.error ?? '', ...run.warnings.slice(0, 5).map(w => h('div', { className: 'small' }, w)), run.warnings.length > 5 ? h('div', { className: 'small' }, `…${run.warnings.length - 5} more`) : null))));
}

(async () => {
  core = await loadCore();
  for (const event of ['pointerdown', 'keydown']) document.addEventListener(event, () => void touch());
  window.addEventListener('pagehide', () => vault?.free());
  await renderVault();
  await renderRecipes();
  await renderRuns();
  chrome.storage.onChanged.addListener(changes => {
    if (changes.recipes) void renderRecipes();
    if (changes.vault && !vault) void renderVault();
  });
  setInterval(() => { if (document.visibilityState === 'visible') void renderRuns(); }, 3_000);
})().catch(e => say((e as Error).message, true));
