// Builds DOM without HTML parsing. Scraped values are untrusted, so they only ever
// become text nodes; the page CSP (Trusted Types, no policies) blocks innerHTML too.

type Child = Node | string | number | null | undefined | false;
type Props<K extends keyof HTMLElementTagNameMap> = Partial<Omit<HTMLElementTagNameMap[K], 'innerHTML' | 'outerHTML' | 'style'>> & Record<`on${string}`, unknown>;

export function h<K extends keyof HTMLElementTagNameMap>(tag: K, props: Partial<Props<K>> = {}, ...children: Child[]): HTMLElementTagNameMap[K] {
  const el = document.createElement(tag);
  for (const [key, value] of Object.entries(props)) {
    if (key === 'innerHTML' || key === 'outerHTML') throw new Error('HTML strings are not allowed');
    if (key.startsWith('on') && typeof value === 'function') el.addEventListener(key.slice(2), value as EventListener);
    else (el as unknown as Record<string, unknown>)[key] = value;
  }
  for (const child of children) {
    if (child === null || child === undefined || child === false) continue;
    el.append(typeof child === 'object' ? child : String(child));
  }
  return el;
}

export function $(id: string): HTMLElement {
  const el = document.getElementById(id);
  if (!el) throw new Error(`Missing element #${id}`);
  return el;
}
export const input = (id: string) => $(id) as HTMLInputElement;
export const select = (id: string) => $(id) as HTMLSelectElement;

/** Sends a request to the service worker; throws its error message. */
export async function send<T = unknown>(message: object): Promise<T> {
  const reply = (await chrome.runtime.sendMessage(message)) as { error?: string } | undefined;
  if (reply && typeof reply === 'object' && 'error' in reply && reply.error) throw new Error(reply.error);
  return reply as T;
}

export function download(name: string, type: string, content: string) {
  const url = URL.createObjectURL(new Blob([content], { type }));
  const a = h('a', { href: url, download: name });
  document.body.append(a);
  a.click();
  a.remove();
  setTimeout(() => URL.revokeObjectURL(url), 10_000);
}
