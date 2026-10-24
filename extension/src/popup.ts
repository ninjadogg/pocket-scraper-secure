// Popup: build or edit a recipe. Picks happen on the page; the service worker records
// them into the draft, so the popup can close and reopen in between.
import { loadCore } from './core.js';
import { $, h, input, select, send } from './dom.js';
import * as store from './store.js';
import { startTour, tourOnce, type TourStep } from './tour.js';
import type { Draft, FieldKind, PickMode, Recipe, RecipeEntry, Step } from './types.js';

let draft: Draft = store.emptyDraft();
let recipes: Record<string, RecipeEntry> = {};
const corePromise = loadCore();

function say(text: string, bad = false) {
  const m = $('message');
  m.textContent = text;
  m.classList.toggle('bad', bad);
}

function readInputs() {
  draft.name = input('name').value;
  draft.url = input('url').value;
  draft.rowSelector = input('rowSelector').value;
  draft.maxPages = Number(input('maxPages').value) || 1;
  draft.scheduleMinutes = Number(select('schedule').value) || 0;
  draft.useNativeHost = input('useNativeHost').checked;
  if (draft.workflow) draft.workflow.repeatCount = Number(input('repeatCount').value) || 1;
}

async function persist() {
  readInputs();
  await store.setDraft(draft);
}

function render() {
  input('name').value = draft.name ?? '';
  input('url').value = draft.url ?? '';
  input('rowSelector').value = draft.rowSelector ?? '';
  input('maxPages').value = String(draft.maxPages ?? 1);
  select('schedule').value = String(draft.scheduleMinutes ?? 0);
  input('useNativeHost').checked = Boolean(draft.useNativeHost);
  select('mode').value = draft.workflow ? 'workflow' : 'pages';
  $('pagesControls').hidden = Boolean(draft.workflow);
  $('workflowControls').hidden = !draft.workflow;
  ($('pickNext') as HTMLButtonElement).disabled = Boolean(draft.workflow);
  input('repeatCount').value = String(draft.workflow?.repeatCount ?? 1);

  const kinds: Record<string, string> = { text: 'text', href: 'link', value: 'value', html: 'HTML' };
  $('fields').replaceChildren(...(draft.fields.length ? draft.fields.map((f, i) => {
    const selector = h('input', { value: f.selector, title: 'Where it is on the page (a CSS selector; edit if needed)', ariaLabel: `Where ${f.name} is on the page`, onchange: () => { f.selector = selector.value; void persist(); } });
    return h('div', { className: 'item field-item' },
      h('span', { className: 'name', title: `${f.name} (${kinds[f.kind] ?? f.kind})` }, f.name), h('span', { className: 'small' }, kinds[f.kind] ?? f.kind), selector,
      h('button', { className: 'tight danger', title: `Remove ${f.name}`, ariaLabel: `Remove field ${f.name}`, onclick: async () => { draft.fields.splice(i, 1); await persist(); render(); } }, '×'));
  }) : [h('p', { className: 'small' }, 'No fields yet.')]));
  $('fieldCount').textContent = draft.fields.length ? `${draft.fields.length} field${draft.fields.length === 1 ? '' : 's'}` : '';
  $('step1').classList.toggle('done', Boolean(draft.url && draft.rowSelector));
  $('step2').classList.toggle('done', draft.fields.length > 0);

  const removable = (label: string, value: string, onchange: (v: string) => void, remove: () => void) => {
    const selector = h('input', { value, ariaLabel: label, onchange: () => { onchange(selector.value); void persist(); } });
    return h('div', { className: 'item' }, h('div', { className: 'small' }, label),
      h('div', { className: 'row' }, selector, h('button', { className: 'tight danger', title: 'Remove', ariaLabel: `Remove: ${label}`, onclick: async () => { remove(); await persist(); render(); } }, '×')));
  };
  const pageItems: HTMLElement[] = draft.actions.map((a, i) =>
    removable(`Click ${i + 1}, before collecting`, a.selector, v => { a.selector = v; }, () => draft.actions.splice(i, 1)));
  if (draft.nextSelector) {
    pageItems.push(removable('Next / Load more button', draft.nextSelector, v => { draft.nextSelector = v; }, () => { draft.nextSelector = ''; }));
  }
  $('actions').replaceChildren(...pageItems);

  const steps = draft.workflow?.steps ?? [];
  $('steps').replaceChildren(...steps.map((step, i) => stepItem(step, i, steps)));
  if (draft.lastPick) say(`Selected ${draft.lastPick}`);
}

function stepItem(step: Step, i: number, steps: Step[]): HTMLElement {
  const title = step.type === 'extract' ? 'Collect the fields' : step.type === 'click' ? 'Click' : 'Wait';
  const item = h('div', { className: 'item' }, h('div', {}, `${i + 1}. ${title}`));
  if (step.type === 'click') {
    const selector = h('input', { value: step.selector, title: 'Click target', ariaLabel: `Step ${i + 1}: what to click`, onchange: () => { step.selector = selector.value; void persist(); } });
    item.append(selector);
  }
  if (step.type !== 'extract') {
    const wait = h('input', { type: 'number', min: '0', max: '30000', value: String(step.waitMs), title: 'Wait in milliseconds', ariaLabel: `Step ${i + 1}: wait in milliseconds`, onchange: () => { step.waitMs = Number(wait.value); void persist(); } });
    item.append(h('div', { className: 'small' }, step.type === 'click' ? 'Then wait (ms)' : 'Wait (ms)'), wait);
  }
  const move = (delta: number) => async () => {
    [steps[i], steps[i + delta]] = [steps[i + delta], steps[i]];
    await persist();
    render();
  };
  item.append(h('div', { className: 'row' },
    h('button', { disabled: i === 0, ariaLabel: `Move step ${i + 1} up`, onclick: move(-1) }, 'Up'),
    h('button', { disabled: i === steps.length - 1, ariaLabel: `Move step ${i + 1} down`, onclick: move(1) }, 'Down'),
    h('button', { className: 'danger', ariaLabel: `Remove step ${i + 1}`, onclick: async () => { steps.splice(i, 1); await persist(); render(); } }, 'Remove')));
  return item;
}

async function activeTab(): Promise<chrome.tabs.Tab> {
  const [tab] = await chrome.tabs.query({ active: true, currentWindow: true });
  if (!tab?.id || !/^https?:\/\//.test(tab.url ?? '')) throw new Error('Open an http(s) website in this tab first.');
  return tab;
}

async function pick(mode: PickMode) {
  try {
    await persist();
    const tab = await activeTab();
    if (!draft.url) {
      draft.url = tab.url;
      await store.setDraft(draft);
    }
    const fieldName = input('fieldName').value.trim() || `field_${draft.fields.length + 1}`;
    await send({ type: 'startPicker', tabId: tab.id, mode, fieldName, kind: select('kind').value as FieldKind });
    window.close();
  } catch (e) {
    say((e as Error).message, true);
  }
}

/** Whether the draft holds work the person would lose by switching recipes. */
function unsaved(): boolean {
  readInputs();
  const shape = (r: Partial<Recipe>) => JSON.stringify([r.name ?? '', r.url ?? '', r.rowSelector ?? '', r.fields ?? [], r.actions ?? [], r.nextSelector ?? '', r.workflow ?? null]);
  const saved = draft.id ? recipes[draft.id]?.recipe : undefined;
  if (saved) return shape(recipeInput()) !== shape(saved);
  return Boolean(draft.rowSelector || draft.fields.length || draft.actions.length || draft.name?.trim());
}

/** The recipe as the core expects it; the core validates and fills defaults. */
function recipeInput(): Partial<Recipe> {
  return {
    ...(draft.id ? { id: draft.id } : {}),
    name: draft.name ?? '',
    url: draft.url ?? '',
    fields: draft.fields,
    actions: draft.workflow ? [] : draft.actions,
    rowSelector: draft.rowSelector ?? '',
    nextSelector: draft.workflow ? '' : draft.nextSelector ?? '',
    maxPages: draft.workflow ? 1 : draft.maxPages ?? 1,
    scheduleMinutes: draft.scheduleMinutes ?? 0,
    workflow: draft.workflow ?? null,
    useNativeHost: Boolean(draft.useNativeHost)
  };
}

/** Problems a person can fix, in plain words, before the core's own checks. Marks
 * the inputs and focuses the first. */
function problems(): string[] {
  const found: [string, string][] = [];
  if (!draft.name?.trim()) found.push(['name', 'Name this recipe (step 1).']);
  if (!draft.url) found.push(['url', 'Add the starting page (step 1): open the site and click This page.']);
  else if (!/^https?:\/\//i.test(draft.url)) found.push(['url', 'The starting page must start with http:// or https://.']);
  if (!draft.rowSelector) found.push(['rowSelector', 'Pick one record on the page (step 1).']);
  else if (!validSelector(draft.rowSelector)) found.push(['rowSelector', '“One record is…” isn’t a page selector. Use Pick on page instead.']);
  if (!draft.fields.length) found.push(['fieldName', 'Add at least one field (step 2): type a name, click Pick, then click a value on the page.']);
  for (const id of ['name', 'url', 'rowSelector', 'fieldName']) $(id).removeAttribute('aria-invalid');
  for (const [id] of found) $(id).setAttribute('aria-invalid', 'true');
  if (found.length) $(found[0][0]).focus();
  return found.map(([, text]) => text);
}

const validSelector = (s: string) => { try { return CSS.supports(`selector(${s})`); } catch { return false; } };

async function save(runAfter: boolean) {
  try {
    readInputs();
    const missing = problems();
    if (missing.length) return say(missing.join('\n'), true);
    const core = await corePromise;
    const json = JSON.stringify(recipeInput());
    core.validateRecipe(json);
    const origin = core.recipeOrigin(json);
    // Must run in the click's user gesture: Chrome shows its own permission prompt.
    if (!(await chrome.permissions.request({ origins: [origin + '/*'] }))) throw new Error(`Access to ${origin} is needed to run this recipe.`);
    const saved = await send<Recipe>({ type: 'saveRecipe', recipe: recipeInput() });
    draft.id = saved.id;
    draft.lastPick = undefined;
    await store.setDraft(draft);
    await loadRecipes();
    select('saved').value = saved.id;
    if (runAfter) {
      await send({ type: 'runNow', id: saved.id });
      say(draft.useNativeHost ? 'Running in the private browser. Results appear in the dashboard.' : 'Running in a background tab. Results appear in the dashboard.');
    } else say('Saved.');
  } catch (e) {
    say((e as Error).message, true);
  }
}

async function loadRecipes() {
  recipes = await store.getRecipes();
  select('saved').replaceChildren(h('option', { value: '' }, 'New recipe'),
    ...Object.values(recipes).sort((a, b) => a.recipe.name.localeCompare(b.recipe.name)).map(e => h('option', { value: e.recipe.id }, e.recipe.name)));
  if (draft.id && recipes[draft.id]) select('saved').value = draft.id;
}

async function showStatus() {
  const vault = await store.getVaultHeader();
  const host = await send<{ installed: boolean }>({ type: 'hostStatus' }).catch(() => ({ installed: false }));
  $('status').textContent = vault ? 'Vault ready: results are stored encrypted.' : '';
  $('vaultBanner').hidden = Boolean(vault);
  input('useNativeHost').disabled = !host.installed && !draft.useNativeHost;
  $('hostNote').textContent = host.installed ? 'Its own profile on this computer: it can run while Chrome is closed.' : 'The helper app isn’t installed, so runs use a background tab in this browser.';
}

function bind() {
  for (const id of ['name', 'url', 'rowSelector', 'maxPages', 'schedule', 'useNativeHost', 'repeatCount']) $(id).addEventListener('change', () => void persist());
  $('useTab').onclick = async () => {
    try { input('url').value = (await activeTab()).url ?? ''; await persist(); } catch (e) { say((e as Error).message, true); }
  };
  $('pickRow').onclick = () => pick('row');
  $('pickField').onclick = () => pick('field');
  $('pickAction').onclick = () => pick('action');
  $('pickStepClick').onclick = () => pick('action');
  $('pickNext').onclick = () => pick('next');
  $('addWait').onclick = async () => { draft.workflow?.steps.push({ type: 'wait', waitMs: 1000 }); await persist(); render(); };
  $('addExtract').onclick = async () => { draft.workflow?.steps.push({ type: 'extract' }); await persist(); render(); };
  select('mode').onchange = async () => {
    readInputs();
    draft.workflow = select('mode').value === 'workflow'
      ? { steps: [...draft.actions.map(a => ({ type: 'click' as const, selector: a.selector, waitMs: a.waitMs })), { type: 'extract' }], repeatCount: 1 }
      : null;
    await store.setDraft(draft);
    render();
  };
  select('saved').onchange = async () => {
    if (unsaved() && !confirm(`Discard the unsaved recipe “${draft.name?.trim() || 'New recipe'}”?`)) {
      select('saved').value = draft.id && recipes[draft.id] ? draft.id : '';
      return;
    }
    const entry = recipes[select('saved').value];
    draft = entry ? { ...entry.recipe, fields: [...entry.recipe.fields], actions: [...entry.recipe.actions] } : store.emptyDraft();
    await store.setDraft(draft);
    say('');
    render();
  };
  $('save').onclick = () => save(false);
  $('saveRun').onclick = () => save(true);
  $('dashboard').onclick = () => chrome.runtime.openOptionsPage();
  $('setupVault').onclick = () => chrome.runtime.openOptionsPage();
  input('rowSelector').addEventListener('change', () => {
    const v = input('rowSelector').value.trim();
    if (v && !validSelector(v)) say('That isn’t a page selector. Use Pick on page instead.', true);
  });
}

const TOUR: TourStep[] = [
  { target: '#useTab', title: 'Start on the site', body: 'Open the page you want to collect from, then click This page.' },
  { target: '#pickRow', title: 'Pick one record', body: 'Click Pick on page, then click one repeating item: a card, a list item or a table row. Each one becomes a record. The popup closes while you pick; reopen it to carry on.' },
  { target: '#addField', title: 'Add the fields', body: 'Name a field (say, price), click Pick, then click that value inside one record. Repeat for each value you want.' },
  { target: '#saveRow', title: 'Save and run', body: 'Save asks Chrome to allow this site. Results are encrypted as they arrive; read them in the Dashboard. Click Tour at the top to see this again.' }
];

/** After a pick the popup reopens: bring the next thing to do into view. */
function focusNext() {
  const kind = draft.lastPick?.split(':')[0];
  if (kind === 'row' || kind === 'field') {
    $('step2').scrollIntoView({ block: 'nearest' });
    input('fieldName').focus();
  } else if (kind === 'next' || kind === 'action') {
    $('step3').scrollIntoView({ block: 'nearest' });
  }
}

(async () => {
  draft = await store.getDraft();
  bind();
  $('tour').onclick = () => startTour('popup', TOUR);
  await loadRecipes();
  render();
  await showStatus();
  focusNext();
  await tourOnce('popup', TOUR);
})().catch(e => say((e as Error).message, true));
