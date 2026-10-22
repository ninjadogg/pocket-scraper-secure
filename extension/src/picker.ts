// Injected into the page's isolated world only after the person clicks a Pick button.
// It builds a CSS selector from the element they click and sends it back once, with
// the nonce the service worker gave it. Its banner and highlight live in a closed
// shadow root, and it never changes the page's own elements.

interface PickerOptions { mode: 'row' | 'field' | 'action' | 'next'; nonce: string; rowSelector: string; kind: string }
type PickerGlobal = { __pssPicker?: (o: PickerOptions) => void; __pssPickerCleanup?: () => void };
const g = globalThis as unknown as PickerGlobal;

g.__pssPicker = (options: PickerOptions) => {
  g.__pssPickerCleanup?.();
  const { mode, nonce, rowSelector, kind } = options;
  let root: ParentNode = document;
  let hovered: Element | null = null;

  const host = document.createElement('pss-picker');
  const shadow = host.attachShadow({ mode: 'closed' });
  const banner = document.createElement('div');
  const box = document.createElement('div');
  Object.assign(host.style, { all: 'initial', position: 'fixed', inset: '0', zIndex: '2147483647', pointerEvents: 'none' });
  Object.assign(banner.style, {
    position: 'fixed', top: '8px', left: '8px', padding: '9px 12px', background: '#131a27', color: '#fff',
    borderRadius: '8px', font: '13px system-ui, sans-serif', boxShadow: '0 2px 10px #0006'
  });
  Object.assign(box.style, { position: 'fixed', border: '3px solid #f97316', borderRadius: '3px', display: 'none' });
  banner.textContent = `Pocket Scraper Secure: click the ${{ row: 'record container', field: 'field', action: 'element to click', next: 'Next button' }[mode]}. Esc cancels.`;
  shadow.append(banner, box);
  document.documentElement.append(host);

  const count = (selector: string) => { try { return root.querySelectorAll(selector).length; } catch { return 0; } };
  const stable = (s: string) => s.length < 60 && !/\d{5,}/.test(s);

  function selectorFor(el: Element, unique: boolean): string {
    const esc = (s: string) => CSS.escape(s);
    if (unique && el.id && stable(el.id)) return `#${esc(el.id)}`;
    const tag = el.tagName.toLowerCase();
    const classes = [...el.classList].filter(stable);
    const choices: string[] = [];
    for (let n = Math.min(3, classes.length); n >= 1; n--) choices.push(tag + classes.slice(0, n).map(c => `.${esc(c)}`).join(''));
    choices.push(tag);
    if (!unique) {
      const repeated = choices.find(s => count(s) >= 2 && count(s) <= 300);
      if (repeated) return repeated;
      if (el.id && stable(el.id)) return `#${esc(el.id)}`;
    }
    const single = choices.find(s => count(s) === 1);
    if (single) return single;
    const parts: string[] = [];
    for (let node: Element | null = el; node && node !== root && parts.length < 12; node = node.parentElement) {
      if (node.id && stable(node.id)) {
        parts.unshift(`#${esc(node.id)}`);
        break;
      }
      const same = [...(node.parentElement?.children ?? [])].filter(s => s.tagName === node!.tagName);
      parts.unshift(node.tagName.toLowerCase() + (same.length > 1 ? `:nth-of-type(${same.indexOf(node) + 1})` : ''));
    }
    return parts.join(' > ');
  }

  function targetOf(e: Event): Element | null {
    const target = e.target;
    if (!(target instanceof Element) || target === host) return null;
    let t: Element = target;
    if (mode === 'row') return t.closest('article,li,tr') ?? t;
    if (kind === 'href') t = t.closest('a[href]') ?? t;
    if (mode === 'action' || mode === 'next') t = t.closest('a,button,[role="button"],input[type="submit"]') ?? t;
    return t;
  }

  function onMove(e: MouseEvent) {
    if (!e.isTrusted) return;
    const el = targetOf(e);
    if (!el || el === hovered) return;
    hovered = el;
    const r = el.getBoundingClientRect();
    Object.assign(box.style, { display: 'block', left: `${r.left - 3}px`, top: `${r.top - 3}px`, width: `${r.width}px`, height: `${r.height}px` });
  }

  function onClick(e: MouseEvent) {
    // Synthetic clicks from page scripts are ignored.
    if (!e.isTrusted) return;
    const el = targetOf(e);
    if (!el) return;
    e.preventDefault();
    e.stopImmediatePropagation();
    if (mode === 'field' && rowSelector) {
      let container: Element | null = null;
      try { container = el.closest(rowSelector); } catch { /* invalid container selector */ }
      if (!container) {
        banner.textContent = 'Click a field inside the selected record container.';
        return;
      }
      root = container;
    }
    // Fields are relative to their container, and so is their match count.
    const selector = selectorFor(el, mode !== 'field' && mode !== 'row');
    const matches = count(selector);
    cleanup();
    void chrome.runtime.sendMessage({ type: 'pick', nonce, selector, matches }).catch(() => {});
  }

  function onKey(e: KeyboardEvent) {
    if (e.isTrusted && e.key === 'Escape') cleanup();
  }

  function cleanup() {
    host.remove();
    document.removeEventListener('mousemove', onMove, true);
    document.removeEventListener('click', onClick, true);
    document.removeEventListener('keydown', onKey, true);
    delete g.__pssPickerCleanup;
  }

  document.addEventListener('mousemove', onMove, true);
  document.addEventListener('click', onClick, true);
  document.addEventListener('keydown', onKey, true);
  g.__pssPickerCleanup = cleanup;
};
