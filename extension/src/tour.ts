// A product tour: a spotlight on one control at a time with a short explanation, and
// Back / Next / Skip. Built from DOM nodes and text only (no HTML strings), so it
// works under the pages' Trusted Types CSP. Each tour shows itself once (marked seen
// as it starts: a popup can close mid-tour); a Tour button replays it. While it is
// open the rest of the page is inert, and focus returns where it was afterwards.
import { h } from './dom.js';

export interface TourStep { target: string; title: string; body: string }

const seenKey = 'toursSeen';

async function seen(name: string): Promise<boolean> {
  const { [seenKey]: s } = await chrome.storage.local.get(seenKey);
  return Boolean((s as Record<string, boolean> | undefined)?.[name]);
}

async function markSeen(name: string) {
  const { [seenKey]: s } = await chrome.storage.local.get(seenKey);
  await chrome.storage.local.set({ [seenKey]: { ...(s as Record<string, boolean> | undefined), [name]: true } });
}

/** Starts the tour the first time this page is opened. */
export async function tourOnce(name: string, steps: TourStep[]) {
  if (!(await seen(name))) startTour(name, steps);
}

let active: (() => void) | null = null;

export function startTour(name: string, allSteps: TourStep[]) {
  active?.();
  // Steps whose control isn't on screen right now (hidden by state) are left out.
  const steps = allSteps.filter(s => {
    const el = document.querySelector(s.target);
    return el instanceof HTMLElement && el.getClientRects().length > 0;
  });
  if (!steps.length) return;
  void markSeen(name);
  const returnFocus = document.activeElement as HTMLElement | null;
  const others = [...document.body.children].filter(el => !el.hasAttribute('inert')) as HTMLElement[];
  for (const el of others) el.inert = true;
  let i = 0;
  const spot = h('div', { className: 'tour-spot' });
  const title = h('h3', { id: 'tour-title' });
  const body = h('p', { id: 'tour-body' });
  const progress = h('span', { className: 'small' });
  const back = h('button', { onclick: () => go(i - 1) }, 'Back');
  const next = h('button', { className: 'primary', onclick: () => (i === steps.length - 1 ? end() : go(i + 1)) });
  const skip = h('button', { className: 'tour-skip', onclick: () => end() }, 'Skip tour');
  const card = h('div', { className: 'tour-card', role: 'dialog', ariaModal: 'true' },
    title, body, h('div', { className: 'tour-actions' }, progress, skip, back, next));
  card.setAttribute('aria-labelledby', 'tour-title');
  card.setAttribute('aria-describedby', 'tour-body');
  const layer = h('div', { className: 'tour' }, spot, card);
  // Clicks outside the card don't fall through to the page while the tour is open.
  layer.addEventListener('click', e => { if (e.target === layer) e.stopPropagation(); });
  document.body.append(layer);

  function place() {
    const el = document.querySelector(steps[i].target) as HTMLElement | null;
    if (!el) return;
    const r = el.getBoundingClientRect();
    const pad = 6;
    Object.assign(spot.style, { left: `${r.left - pad}px`, top: `${r.top - pad}px`, width: `${r.width + pad * 2}px`, height: `${r.height + pad * 2}px` });
    const vw = document.documentElement.clientWidth;
    const vh = window.innerHeight;
    const cw = Math.min(320, vw - 16);
    card.style.width = `${cw}px`;
    const ch = card.offsetHeight;
    const below = r.bottom + pad + 10;
    const top = below + ch <= vh - 8 ? below : Math.max(8, r.top - pad - 10 - ch);
    const left = Math.min(Math.max(8, r.left), vw - cw - 8);
    Object.assign(card.style, { left: `${left}px`, top: `${top}px` });
  }

  function go(n: number) {
    i = Math.max(0, Math.min(steps.length - 1, n));
    const step = steps[i];
    title.textContent = step.title;
    body.textContent = step.body;
    progress.textContent = `${i + 1} of ${steps.length}`;
    back.disabled = i === 0;
    next.textContent = i === steps.length - 1 ? 'Done' : 'Next';
    document.querySelector(step.target)?.scrollIntoView({ block: 'center' });
    place();
    next.focus();
  }

  const onKey = (e: KeyboardEvent) => {
    if (e.key === 'Escape') end();
    else if (e.key === 'ArrowRight') go(i + 1);
    else if (e.key === 'ArrowLeft') go(i - 1);
    else return;
    e.preventDefault();
  };
  const onMove = () => place();
  document.addEventListener('keydown', onKey, true);
  window.addEventListener('resize', onMove);
  window.addEventListener('scroll', onMove, true);

  function end() {
    document.removeEventListener('keydown', onKey, true);
    window.removeEventListener('resize', onMove);
    window.removeEventListener('scroll', onMove, true);
    layer.remove();
    for (const el of others) el.inert = false;
    active = null;
    returnFocus?.focus();
  }
  active = end;
  go(0);
}
