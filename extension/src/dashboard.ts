// Dashboard: the only place records are decrypted. The unlocked key lives in this
// page's WebAssembly memory and is freed (zeroized) on lock, on inactivity and when
// the page closes.
import type { UnlockedVault } from '../build/core/pocket_scraper_core_wasm.js';
import { loadCore, type Core } from './core.js';
import { $, download, h, send } from './dom.js';
import * as store from './store.js';
import type { Batch, PlainRecord, RecipeEntry, Runner, RunRecord } from './types.js';

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
  void renderRecipes();
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
        say('');
        await touch();
        await renderVault();
        if (selected) await showRecords(selected);
      } catch (e) { say((e as Error).message, true); }
    };
    pass.addEventListener('keydown', e => { if (e.key === 'Enter') void unlock(); });
    box.replaceChildren(h('strong', {}, 'Vault locked'), ...fingerprintLine(header), h('div', { className: 'row' }, pass, h('button', { className: 'primary tight', onclick: unlock }, 'Unlock')));
    return;
  }
  const oldPass = h('input', { type: 'password', autocomplete: 'current-password', placeholder: 'Current passphrase' });
  const newPass = h('input', { type: 'password', autocomplete: 'new-password', placeholder: 'New passphrase' });
  box.replaceChildren(
    h('div', { className: 'row' }, h('strong', {}, 'Vault unlocked'), h('button', { className: 'tight', onclick: lock }, 'Lock now')),
    ...fingerprintLine((await store.getVaultHeader()) ?? header),
    h('details', {}, h('summary', {}, 'Change passphrase'), h('div', { className: 'row' }, oldPass, newPass, h('button', { className: 'tight', onclick: async () => {
      try {
        const current = await store.getVaultHeader();
        if (!current) throw new Error('The vault is gone.');
        await store.setVaultHeader(core.changePassphrase(current, oldPass.value, newPass.value));
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
  if (vault) {
    actions.append(act('Export for runner', async () => {
      const { runTimeoutMs } = await store.getSettings();
      const header = await store.getVaultHeader();
      const file = `${r.name.replace(/[^\w.-]+/g, '_').slice(0, 60) || 'recipe'}.pssbundle`;
      download(file, 'application/json', vault!.exportBundle(JSON.stringify(r), new Date().toISOString(), runTimeoutMs));
      say(`Exported ${file}. On the runner: pocket-scraper-runner add ${file} --trust ${header ? core.vaultFingerprint(header) : ''}`);
    }));
  }
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
  const names = entry.recipe.fields.map(f => f.name);
  const shown = records.slice(0, 500);
  const safeName = entry.recipe.name.replace(/[^\w.-]+/g, '_').slice(0, 60) || 'records';
  $('records').replaceChildren(
    h('div', { className: 'actions' },
      h('button', { onclick: () => download(`${safeName}.csv`, 'text/csv', core.toCsv(JSON.stringify(names), JSON.stringify(records))) }, 'Export CSV'),
      h('button', { onclick: () => download(`${safeName}.json`, 'application/json', JSON.stringify(records, null, 2)) }, 'Export JSON')),
    h('p', { className: 'small' }, `${records.length} record(s)${records.length > shown.length ? `, newest ${shown.length} shown` : ''}${duplicates.length ? `; ${duplicates.length} repeat copies from later runs removed` : ''}.`),
    ...(unreadable ? [h('p', { className: 'message bad' }, `${unreadable} record(s) could not be opened: they were modified or belong to another vault.`)] : []),
    h('div', { className: 'scroll' }, h('table', {},
      h('tr', {}, ...['Observed', ...names, 'Page'].map(t => h('th', {}, t))),
      ...shown.map(r => h('tr', {}, h('td', {}, new Date(r.observedAt).toLocaleString()), ...names.map(n => h('td', {}, r.fields[n] ?? '')), h('td', {}, r.pageUrl))))));
}

async function renderRunners() {
  const runners = await store.getRunners();
  const name = h('input', { placeholder: 'Name, e.g. build-server' });
  const key = h('input', { placeholder: 'Runner key (from pocket-scraper-runner init)' });
  const shown = h('p', { className: 'small' });
  key.addEventListener('input', () => {
    try { shown.textContent = `Fingerprint ${core.keyFingerprint(key.value)}: check it matches what the runner printed.`; }
    catch { shown.textContent = ''; }
  });
  $('runners').replaceChildren(
    ...runners.map(r => h('div', { className: 'row' },
      h('span', {}, r.name), h('code', { className: 'small' }, r.fingerprint),
      h('button', { className: 'tight', onclick: async () => {
        if (!confirm(`Remove runner “${r.name}”? Its future results will be refused.`)) return;
        await store.setRunners((await store.getRunners()).filter(x => x.key !== r.key));
        await renderRunners();
      } }, 'Remove'))),
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

/** Imports result batch files: signed by an enrolled runner, sealed to this vault,
 * each at most once. Works while the vault is locked (records stay sealed). */
async function importBatches(files: FileList) {
  const header = await store.getVaultHeader();
  if (!header) return say('Create the vault first.', true);
  const runners = await store.getRunners();
  const recipes = await store.getRecipes();
  const lines: string[] = [];
  for (const file of [...files]) {
    try {
      if (file.size > 8 * 1024 * 1024) throw new Error('larger than 8 MB');
      const batch = JSON.parse(core.openBatch(await file.text(), JSON.stringify(runners.map(r => r.key)), header)) as Batch;
      const runner = runners.find(r => r.key === batch.runnerKey)!;
      if (!recipes[batch.recipeId]) throw new Error('its recipe no longer exists here');
      if (!(await store.claimBatch(batch.batchId))) { lines.push(`${file.name}: already imported`); continue; }
      await store.addRecords(batch.recipeId, batch.runId, batch.records);
      await store.putRun({
        id: batch.runId, recipeId: batch.recipeId, via: 'runner', runner: runner.name, trigger: 'schedule',
        startedAt: batch.startedAt, finishedAt: batch.finishedAt, status: batch.status, rowCount: batch.rowCount,
        warnings: batch.warnings, ...(batch.error ? { error: batch.error } : {})
      });
      lines.push(`${file.name}: ${batch.status.replace('_', ' ')}, ${batch.rowCount} rows from ${runner.name}`);
    } catch (e) {
      lines.push(`${file.name}: refused (${(e as Error).message})`);
    }
  }
  $('importMessage').textContent = lines.join(' · ');
  await renderRecipes();
  await renderRuns();
  if (selected && vault) await showRecords(selected);
}

async function renderRuns() {
  const recipes = await store.getRecipes();
  const runs: RunRecord[] = await store.listRuns(50);
  $('runs').replaceChildren(
    h('tr', {}, ...['Started', 'Recipe', 'Via', 'Status', 'Rows', 'Details'].map(t => h('th', {}, t))),
    ...runs.map(run => h('tr', {},
      h('td', {}, new Date(run.startedAt).toLocaleString()),
      h('td', {}, recipes[run.recipeId]?.recipe.name ?? '(deleted)'),
      h('td', {}, `${run.via === 'host' ? 'Native host' : run.via === 'runner' ? `Runner ${run.runner ?? ''}` : 'Tab'}, ${run.trigger}`),
      h('td', { className: `status-${run.status}` }, run.status.replace('_', ' ')),
      h('td', {}, run.rowCount ?? ''),
      h('td', {}, run.error ?? '', ...run.warnings.slice(0, 5).map(w => h('div', { className: 'small' }, w)), run.warnings.length > 5 ? h('div', { className: 'small' }, `…${run.warnings.length - 5} more`) : null))));
}

(async () => {
  core = await loadCore();
  for (const event of ['pointerdown', 'keydown']) document.addEventListener(event, () => void touch());
  window.addEventListener('pagehide', () => vault?.free());
  await renderVault();
  await renderRecipes();
  await renderRunners();
  await renderRuns();
  const picker = $('importBatches') as HTMLInputElement;
  picker.addEventListener('change', () => { if (picker.files?.length) void importBatches(picker.files).finally(() => { picker.value = ''; }); });
  chrome.storage.onChanged.addListener(changes => {
    if (changes.recipes) void renderRecipes();
    if (changes.vault && !vault) void renderVault();
  });
  setInterval(() => { if (document.visibilityState === 'visible') void renderRuns(); }, 3_000);
})().catch(e => say((e as Error).message, true));
