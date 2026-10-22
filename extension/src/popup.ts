// Popup: build or edit a recipe. Picks happen on the page; the service worker records
// them into the draft, so the popup can close and reopen in between.
import { loadCore } from './core.js';
import { $, h, input, select, send } from './dom.js';
import * as store from './store.js';
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

  $('fields').replaceChildren(...draft.fields.map((f, i) => {
    const selector = h('input', { value: f.selector, title: 'CSS selector (edit if needed)', onchange: () => { f.selector = selector.value; void persist(); } });
    return h('div', { className: 'item' },
      h('div', {}, `${f.name} (${f.kind})`),
      h('div', { className: 'row' }, selector, h('button', { className: 'tight', title: 'Remove field', onclick: async () => { draft.fields.splice(i, 1); await persist(); render(); } }, '×')));
  }));

  const pageItems: HTMLElement[] = draft.actions.map((a, i) => {
    const selector = h('input', { value: a.selector, title: 'Clicked before extracting', onchange: () => { a.selector = selector.value; void persist(); } });
    return h('div', { className: 'item' }, h('div', { className: 'small' }, `Click ${i + 1} before extracting`),
      h('div', { className: 'row' }, selector, h('button', { className: 'tight', onclick: async () => { draft.actions.splice(i, 1); await persist(); render(); } }, '×')));
  });
  if (draft.nextSelector) {
    const next = h('input', { value: draft.nextSelector, onchange: () => { draft.nextSelector = next.value; void persist(); } });
    pageItems.push(h('div', { className: 'item' }, h('div', { className: 'small' }, 'Next-page / Load more button'),
      h('div', { className: 'row' }, next, h('button', { className: 'tight', onclick: async () => { draft.nextSelector = ''; await persist(); render(); } }, '×'))));
  }
  $('actions').replaceChildren(...pageItems);

  const steps = draft.workflow?.steps ?? [];
  $('steps').replaceChildren(...steps.map((step, i) => stepItem(step, i, steps)));
  if (draft.lastPick) say(`Selected ${draft.lastPick}`);
}

function stepItem(step: Step, i: number, steps: Step[]): HTMLElement {
  const title = step.type === 'extract' ? 'Extract the fields' : step.type === 'click' ? 'Click' : 'Wait';
  const item = h('div', { className: 'item' }, h('div', {}, `${i + 1}. ${title}`));
  if (step.type === 'click') {
    const selector = h('input', { value: step.selector, title: 'Click target', onchange: () => { step.selector = selector.value; void persist(); } });
    item.append(selector);
  }
  if (step.type !== 'extract') {
    const wait = h('input', { type: 'number', min: '0', max: '30000', value: String(step.waitMs), title: 'Wait in milliseconds', onchange: () => { step.waitMs = Number(wait.value); void persist(); } });
    item.append(h('div', { className: 'small' }, step.type === 'click' ? 'Then wait (ms)' : 'Wait (ms)'), wait);
  }
  const move = (delta: number) => async () => {
    [steps[i], steps[i + delta]] = [steps[i + delta], steps[i]];
    await persist();
    render();
  };
  item.append(h('div', { className: 'row' },
    h('button', { disabled: i === 0, onclick: move(-1) }, 'Up'),
    h('button', { disabled: i === steps.length - 1, onclick: move(1) }, 'Down'),
    h('button', { onclick: async () => { steps.splice(i, 1); await persist(); render(); } }, 'Remove')));
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

async function save(runAfter: boolean) {
  try {
    readInputs();
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
      say('Running in a background tab. Results appear in the dashboard.');
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
  $('status').textContent = vault ? 'Vault ready: results are stored encrypted.' : 'Set up the vault in the dashboard before running recipes.';
  input('useNativeHost').disabled = !host.installed && !draft.useNativeHost;
  $('hostNote').textContent = host.installed ? 'Native host installed: scheduled runs can use a separate, private browser profile.' : 'Native host not installed; runs use a background tab.';
}

function bind() {
  for (const id of ['name', 'url', 'rowSelector', 'maxPages', 'schedule', 'useNativeHost', 'repeatCount']) $(id).addEventListener('change', () => void persist());
  $('useTab').onclick = async () => {
    try { input('url').value = (await activeTab()).url ?? ''; await persist(); } catch (e) { say((e as Error).message, true); }
  };
  $('pickRow').onclick = () => pick('row');
  $('pickField').onclick = () => pick('field');
  $('pickAction').onclick = () => pick('action');
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
    const entry = recipes[select('saved').value];
    draft = entry ? { ...entry.recipe, fields: [...entry.recipe.fields], actions: [...entry.recipe.actions] } : store.emptyDraft();
    await store.setDraft(draft);
    say('');
    render();
  };
  $('save').onclick = () => save(false);
  $('saveRun').onclick = () => save(true);
  $('dashboard').onclick = () => chrome.runtime.openOptionsPage();
}

(async () => {
  draft = await store.getDraft();
  bind();
  await loadRecipes();
  render();
  await showStatus();
})().catch(e => say((e as Error).message, true));
