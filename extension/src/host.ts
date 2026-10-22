// The optional native host, reached over Chrome native messaging (stdio). Chrome
// only starts it for this extension's ID; replies are still validated by the core.
import type { Core } from './core.js';
import type { Recipe, RunOutcome, SealedRecord } from './types.js';

export const HOST_NAME = 'com.pocketscraper.secure';
const PROTOCOL_VERSION = 1;

type HostMessage = { type: string; [key: string]: unknown };

function request(message: object, onMessage: (m: HostMessage, finish: (v: unknown) => void) => void, timeoutMs: number): Promise<unknown> {
  return new Promise(resolve => {
    let port: chrome.runtime.Port;
    try { port = chrome.runtime.connectNative(HOST_NAME); }
    catch (error) { return resolve({ type: 'error', message: String((error as Error).message) }); }
    let settled = false;
    const finish = (value: unknown) => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      try { port.disconnect(); } catch { /* already gone */ }
      resolve(value);
    };
    const timer = setTimeout(() => finish({ type: 'error', message: 'The native host did not answer in time.' }), timeoutMs);
    port.onMessage.addListener(m => onMessage(m as HostMessage, finish));
    port.onDisconnect.addListener(() => finish({ type: 'error', message: chrome.runtime.lastError?.message || 'The native host closed the connection.' }));
    port.postMessage(message);
  });
}

export async function hostStatus(): Promise<{ installed: boolean; version?: string; error?: string }> {
  const reply = (await request({ type: 'hello', version: PROTOCOL_VERSION }, (m, finish) => finish(m), 5_000)) as HostMessage;
  if (reply.type === 'hello' && reply.version === PROTOCOL_VERSION) return { installed: true, version: String(reply.hostVersion) };
  return { installed: false, error: String(reply.message ?? 'Unexpected reply from the native host') };
}

export async function runOnHost(core: Core, recipe: Recipe, vaultHeader: string, runId: string, observedAt: string, runTimeoutMs: number): Promise<RunOutcome> {
  const header = JSON.parse(vaultHeader) as { publicKey: string };
  const records: SealedRecord[] = [];
  const reply = (await request(
    { type: 'run', runId, recipe, publicKey: header.publicKey, runTimeoutMs, observedAt },
    (m, finish) => {
      if (m.runId !== runId && m.type !== 'error') return finish({ type: 'error', message: 'The native host answered for another run.' });
      if (m.type === 'runChunk') {
        try {
          records.push(...(JSON.parse(core.checkSealedRecords(JSON.stringify(m.records))) as SealedRecord[]));
          if (records.length > 5000) finish({ type: 'error', message: 'The native host sent too many records.' });
        } catch (error) {
          finish({ type: 'error', message: `The native host sent malformed records: ${(error as Error).message}` });
        }
      } else finish(m);
    },
    // The host kills its browser at runTimeoutMs + 5 s and then answers; wait longer.
    runTimeoutMs + 30_000
  )) as HostMessage;
  if (reply.type === 'runDone') {
    const warnings = Array.isArray(reply.warnings) ? reply.warnings.filter((w): w is string => typeof w === 'string').slice(0, 101) : [];
    return { ok: true, records, rowCount: Number(reply.rowCount) || 0, warnings };
  }
  if (reply.type === 'runFailed') return { ok: false, message: String(reply.message), attention: reply.attention === true };
  return { ok: false, message: String(reply.message ?? 'The native host failed.'), attention: false };
}

/** New batch files from the bucket the desktop host is configured for. The host sends
 * each file in parts (Chrome's 1 MiB message limit); they are reassembled here and
 * verified by the dashboard like any import. */
export async function collectBatches(retryDeferred = false): Promise<{ ok: true; files: { key: string; text: string }[]; skipped: { key: string; reason: string }[]; more: boolean } | { ok: false; message: string }> {
  const parts = new Map<string, string[]>();
  const skipped: { key: string; reason: string }[] = [];
  const reply = (await request({ type: 'collectBatches', retryDeferred }, (m, finish) => {
    if (m.type === 'batchPart' && typeof m.key === 'string' && typeof m.text === 'string' && Number.isInteger(m.part) && Number.isInteger(m.parts) && (m.parts as number) <= 16) {
      const list = parts.get(m.key) ?? new Array<string>(m.parts as number).fill('');
      list[m.part as number] = m.text;
      parts.set(m.key, list);
    } else if (m.type === 'batchSkipped' && typeof m.key === 'string') {
      skipped.push({ key: m.key, reason: String(m.reason ?? '') });
    } else finish(m);
  }, 180_000)) as HostMessage;
  if (reply.type !== 'collectDone') return { ok: false, message: String(reply.message ?? 'The native host failed.') };
  return { ok: true, files: [...parts].map(([key, list]) => ({ key, text: list.join('') })), skipped, more: reply.more === true };
}

/** `keys` were handled for good; `deferred` are set aside until a retry. */
export async function ackBatches(keys: string[], deferred: string[] = []): Promise<void> {
  if (keys.length || deferred.length) await request({ type: 'ackBatches', keys, deferred }, (m, finish) => finish(m), 10_000);
}

export async function sessionCommand(action: 'open' | 'close', url: string): Promise<{ ok: boolean; message?: string }> {
  const reply = (await request({ type: action === 'open' ? 'openSession' : 'closeSession', url }, (m, finish) => finish(m), 60_000)) as HostMessage;
  if (reply.type === 'sessionOpened' || reply.type === 'sessionClosed') return { ok: true };
  return { ok: false, message: String(reply.message ?? 'The native host failed.') };
}
