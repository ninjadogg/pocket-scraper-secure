// Runs a recipe in an inactive background tab. The Rust planner decides every step;
// this file only performs them. Scripts run in the extension's isolated world on
// origins the person granted, so a run can't reach sites it wasn't given.
import type { Core } from './core.js';
import { extractFields } from './extract.js';
import type { PlanAction, PlanEvent, Recipe, Row } from './types.js';

const NAVIGATION_TIMEOUT_MS = 30_000;
const PAGE_CHANGE_TIMEOUT_MS = 20_000;

type Result = { ok: true; rows: Row[]; warnings: string[] } | { ok: false; message: string; attention: boolean };

export async function runInTab(core: Core, recipe: Recipe, runTimeoutMs: number): Promise<Result> {
  const planner = new core.Planner(JSON.stringify(recipe), runTimeoutMs, PAGE_CHANGE_TIMEOUT_MS);
  const tab = await chrome.tabs.create({ url: 'about:blank', active: false });
  const tabId = tab.id!;
  // Extension API calls keep the service worker alive through long waits.
  const keepAlive = setInterval(() => void chrome.runtime.getPlatformInfo(), 20_000);
  let stopped = false;
  let deadlineTimer: ReturnType<typeof setTimeout> | undefined;
  const deadline = new Promise<Result>(resolve => {
    deadlineTimer = setTimeout(() => resolve({ ok: false, message: `Run timed out after ${Math.round(runTimeoutMs / 1000)}s.`, attention: false }), runTimeoutMs + 5_000);
  });
  const drive = async (): Promise<Result> => {
    let action = JSON.parse(planner.start(performance.now())) as PlanAction;
    for (;;) {
      if (action.type === 'finish') return { ok: true, rows: action.rows, warnings: action.warnings };
      if (action.type === 'fail') return { ok: false, message: action.message, attention: action.attention };
      const event = await perform(tabId, recipe, action);
      // After the deadline the planner is freed and the tab closed: stop quietly.
      if (stopped) return { ok: false, message: 'Run stopped.', attention: false };
      action = JSON.parse(planner.observe(JSON.stringify({ nowMs: Math.round(performance.now()), event }))) as PlanAction;
    }
  };
  const driving = drive().catch((error: unknown): Result => ({ ok: false, message: String((error as Error)?.message ?? error), attention: false }));
  try {
    return await Promise.race([driving, deadline]);
  } finally {
    stopped = true;
    clearTimeout(deadlineTimer);
    clearInterval(keepAlive);
    await chrome.tabs.remove(tabId).catch(() => {});
    // Let an in-flight step finish before freeing the planner it would report to.
    await Promise.race([driving, new Promise(r => setTimeout(r, 35_000))]);
    planner.free();
  }
}

async function perform(tabId: number, recipe: Recipe, action: PlanAction): Promise<PlanEvent> {
  switch (action.type) {
    case 'navigate': {
      // Listen before navigating, so the new page's 'complete' can't be missed.
      const isLoaded = loaded(tabId);
      await chrome.tabs.update(tabId, { url: action.url });
      await isLoaded;
      const [res] = await chrome.scripting.executeScript({ target: { tabId }, func: navigationStatus }).catch(() => [undefined]);
      return { type: 'navigated', status: typeof res?.result === 'number' ? res.result : 0 };
    }
    case 'wait':
      await sleep(action.ms);
      return { type: 'waited' };
    case 'read': {
      try {
        const [res] = await chrome.scripting.executeScript({ target: { tabId }, func: extractFields, args: [{ fields: recipe.fields, rowSelector: recipe.rowSelector }] });
        const r = res?.result;
        if (!r) return { type: 'read', url: '', status: 0, error: 'The page could not be read' };
        return { type: 'read', url: r.url, status: r.status, output: r.output, error: r.error, challenge: Boolean(r.challenge) };
      } catch (error) {
        return { type: 'read', url: '', status: 0, error: accessMessage(error) };
      }
    }
    case 'click': {
      try {
        const [res] = await chrome.scripting.executeScript({ target: { tabId }, func: clickWhenReady, args: [action.selector, action.timeoutMs] });
        return res?.result === true ? { type: 'clicked' } : { type: 'clickFailed', message: typeof res?.result === 'string' ? res.result : `No clickable element matches ${action.selector}` };
      } catch (error) {
        return { type: 'clickFailed', message: accessMessage(error) };
      }
    }
    case 'clickNext': {
      try {
        const [res] = await chrome.scripting.executeScript({ target: { tabId }, func: clickNext, args: [action.selector] });
        const urlBefore = res?.result;
        return typeof urlBefore === 'string' ? { type: 'nextClicked', urlBefore } : { type: 'nextUnavailable' };
      } catch {
        return { type: 'nextUnavailable' };
      }
    }
    default:
      throw new Error(`Unexpected planner action ${(action as PlanAction).type}`);
  }
}

function accessMessage(error: unknown): string {
  const text = String((error as Error)?.message ?? error);
  if (/Cannot access|permission/i.test(text)) return 'This site isn’t allowed for Pocket Scraper Secure, or the page failed to load. Grant access to the site by saving the recipe again.';
  return text;
}

const sleep = (ms: number) => new Promise(resolve => setTimeout(resolve, ms));

/** Resolves when the tab finishes loading something other than about:blank. */
function loaded(tabId: number): Promise<void> {
  return new Promise(resolve => {
    const timer = setTimeout(finish, NAVIGATION_TIMEOUT_MS);
    function finish() {
      clearTimeout(timer);
      chrome.tabs.onUpdated.removeListener(listener);
      resolve();
    }
    function listener(id: number, info: chrome.tabs.OnUpdatedInfo, tab: chrome.tabs.Tab) {
      if (id === tabId && info.status === 'complete' && tab.url !== 'about:blank') finish();
    }
    chrome.tabs.onUpdated.addListener(listener);
  });
}

// Injected functions: self-contained.

function navigationStatus(): number {
  const nav = performance.getEntriesByType('navigation')[0] as PerformanceNavigationTiming & { responseStatus?: number };
  return Number(nav?.responseStatus) || 0;
}

async function clickWhenReady(selector: string, timeoutMs: number): Promise<true | string> {
  const end = Date.now() + timeoutMs;
  for (;;) {
    let el: Element | null;
    try { el = document.querySelector(selector); } catch { return `Invalid selector: ${selector}`; }
    if (el instanceof HTMLElement && el.checkVisibility()) {
      el.click();
      return true;
    }
    if (Date.now() > end) return `No visible element matches ${selector}`;
    await new Promise(r => setTimeout(r, 100));
  }
}

function clickNext(selector: string): string | null {
  let el: Element | null;
  try { el = document.querySelector(selector); } catch { return null; }
  if (!(el instanceof HTMLElement) || !el.checkVisibility()) return null;
  if ((el as HTMLButtonElement).disabled || el.getAttribute('aria-disabled') === 'true') return null;
  const before = location.href;
  el.click();
  return before;
}
