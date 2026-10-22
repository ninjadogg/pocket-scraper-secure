// Loads the Rust core (WebAssembly). Every page and the service worker load their
// own instance; the unlocked vault key only ever exists in the dashboard's.
import init, * as core from '../build/core/pocket_scraper_core_wasm.js';

export type Core = typeof core;
let ready: Promise<Core> | undefined;

export function loadCore(): Promise<Core> {
  ready ??= init({ module_or_path: chrome.runtime.getURL('core.wasm') }).then(() => core);
  return ready;
}
