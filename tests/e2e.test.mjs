// End to end: the built extension in Chrome for Testing (which can load unpacked
// extensions), driven over a debugging pipe. Covers the vault UI, tab runs, sealed
// storage, CSP, the picker, and a run through the real native host.
// Skips when no Chrome for Testing is found (PSS_E2E_BROWSER overrides the path).
import test from 'node:test';
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { cpSync, existsSync, mkdtempSync, mkdirSync, readFileSync, readdirSync, rmSync, writeFileSync } from 'node:fs';
import { createServer } from 'node:http';
import { homedir, tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = fileURLToPath(new URL('..', import.meta.url));
const EXTENSION_ID = readFileSync(join(root, 'extension', 'extension-id.txt'), 'utf8').trim();
const PASS = 'correct horse battery staple';

function findBrowser() {
  if (process.env.PSS_E2E_BROWSER) return process.env.PSS_E2E_BROWSER;
  const cache = join(homedir(), 'Library', 'Caches', 'ms-playwright');
  if (!existsSync(cache)) return null;
  for (const dir of readdirSync(cache).filter(d => d.startsWith('chromium-')).sort().reverse()) {
    const exe = join(cache, dir, 'chrome-mac-arm64', 'Google Chrome for Testing.app', 'Contents', 'MacOS', 'Google Chrome for Testing');
    if (existsSync(exe)) return exe;
  }
  return null;
}

function site() {
  const list = n => `<title>List ${n}</title><ul>${[1, 2].map(i => `<li class="card"><span class="name">Item ${(n - 1) * 2 + i}</span></li>`).join('')}</ul>${n < 3 ? `<a class="next" href="/list?page=${n + 1}">Next</a>` : ''}`;
  // Cards built from plain divs, like quotes.toscrape.com.
  const cards = '<title>Cards</title>' + [1, 2, 3].map(i => `<div class="quote"><span class="text">Quote ${i}</span><small class="author">Author ${i}</small></div>`).join('')
    + '<nav><ul class="pager"><li class="previous"><a href="#">Previous</a></li><li class="next"><a href="#">Next</a></li></ul></nav>';
  const server = createServer((req, res) => {
    const url = new URL(req.url, 'http://x');
    const n = Number(url.searchParams.get('page') || 1);
    res.writeHead(200, { 'Content-Type': 'text/html; charset=utf-8' });
    res.end(url.pathname === '/cards' ? cards : list(n));
  });
  return new Promise(resolve => server.listen(0, '127.0.0.1', () => resolve(server)));
}

/** Minimal CDP client over --remote-debugging-pipe (fd 3 in, fd 4 out). */
function cdp(child) {
  let id = 0;
  let buffer = Buffer.alloc(0);
  const waiting = new Map();
  child.stdio[4].on('data', chunk => {
    buffer = Buffer.concat([buffer, chunk]);
    for (let end; (end = buffer.indexOf(0)) >= 0; buffer = buffer.subarray(end + 1)) {
      const msg = JSON.parse(buffer.subarray(0, end).toString('utf8'));
      if (msg.id && waiting.has(msg.id)) {
        const { resolve, reject } = waiting.get(msg.id);
        waiting.delete(msg.id);
        msg.error ? reject(new Error(msg.error.message)) : resolve(msg.result);
      }
    }
  });
  const call = (method, params = {}, sessionId) => new Promise((resolve, reject) => {
    const msgId = ++id;
    waiting.set(msgId, { resolve, reject });
    child.stdio[3].write(JSON.stringify({ id: msgId, method, params, ...(sessionId ? { sessionId } : {}) }) + '\0');
  });
  return { call };
}

const sleep = ms => new Promise(r => setTimeout(r, ms));

test('the extension collects, seals, shows and protects records end to end', { timeout: 240_000 }, async t => {
  const browserPath = findBrowser();
  if (!browserPath) return t.skip('Chrome for Testing not found');
  const work = mkdtempSync(join(tmpdir(), 'pss-e2e-'));
  const server = await site();
  const base = `http://127.0.0.1:${server.address().port}`;
  // Test-only: grant the local test site up front (automation can't click Chrome's permission prompt).
  const ext = join(work, 'extension');
  cpSync(join(root, 'extension', 'dist'), ext, { recursive: true });
  const manifest = JSON.parse(readFileSync(join(ext, 'manifest.json'), 'utf8'));
  manifest.host_permissions = ['http://127.0.0.1/*'];
  writeFileSync(join(ext, 'manifest.json'), JSON.stringify(manifest));
  // Register the native host for this profile only.
  const profile = join(work, 'profile');
  const hostBinary = join(root, 'target', 'debug', 'pocket-scraper-host');
  const hosts = join(profile, 'NativeMessagingHosts');
  mkdirSync(hosts, { recursive: true });
  writeFileSync(join(hosts, 'com.pocketscraper.secure.json'), JSON.stringify({
    name: 'com.pocketscraper.secure', description: 'test', path: hostBinary, type: 'stdio', allowed_origins: [`chrome-extension://${EXTENSION_ID}/`]
  }));

  const child = spawn(browserPath, [
    '--headless=new', '--remote-debugging-pipe', `--user-data-dir=${profile}`, '--no-first-run', '--no-default-browser-check',
    `--disable-extensions-except=${ext}`, `--load-extension=${ext}`, 'about:blank'
  ], { stdio: ['ignore', 'ignore', 'ignore', 'pipe', 'pipe'], env: { ...process.env, PSS_DATA_DIR: join(work, 'host-data') } });
  const { call } = cdp(child);
  try {
    // The manifest key pins the extension ID the native host allows.
    let worker;
    for (let i = 0; i < 50 && !worker; i++) {
      worker = (await call('Target.getTargets')).targetInfos.find(t => t.type === 'service_worker' && t.url === `chrome-extension://${EXTENSION_ID}/background.js`);
      if (!worker) await sleep(100);
    }
    assert.ok(worker, 'the service worker started under the pinned extension ID');

    const open = async url => {
      const { targetId } = await call('Target.createTarget', { url });
      const { sessionId } = await call('Target.attachToTarget', { targetId, flatten: true });
      await sleep(800);
      const evaluate = async (expression, userGesture = false) => {
        const r = await call('Runtime.evaluate', { expression, awaitPromise: true, returnByValue: true, userGesture }, sessionId);
        if (r.exceptionDetails) throw new Error(r.exceptionDetails.exception?.description ?? r.exceptionDetails.text);
        return r.result.value;
      };
      return { sessionId, evaluate };
    };
    const dash = await open(`chrome-extension://${EXTENSION_ID}/dashboard.html`);
    const waitFor = async (expression, what, ms = 60_000) => {
      const end = Date.now() + ms;
      for (;;) {
        const v = await dash.evaluate(expression);
        if (v) return v;
        if (Date.now() > end) throw new Error(`Timed out waiting for ${what}`);
        await sleep(300);
      }
    };
    const runs = `new Promise(r => { const q = indexedDB.open('pocket-scraper-secure'); q.onsuccess = () => { const g = q.result.transaction('runs').objectStore('runs').getAll(); g.onsuccess = () => r(g.result); }; })`;
    const records = `new Promise(r => { const q = indexedDB.open('pocket-scraper-secure'); q.onsuccess = () => { const g = q.result.transaction('records').objectStore('records').getAll(); g.onsuccess = () => r(g.result); }; })`;
    const send = m => dash.evaluate(`chrome.runtime.sendMessage(${JSON.stringify(m)})`);

    // Runs are refused until a vault exists.
    const early = await send({ type: 'runNow', id: 'nope' });
    assert.match(early.error, /Unknown recipe/);

    // Create the vault through the dashboard UI.
    await waitFor(`document.querySelectorAll('#vault input').length === 2`, 'the create-vault form');
    await dash.evaluate(`(() => { const [a, b] = document.querySelectorAll('#vault input'); a.value = b.value = ${JSON.stringify(PASS)}; document.querySelector('#vault button').click(); })()`);
    await waitFor(`chrome.storage.local.get('vault').then(v => Boolean(v.vault))`, 'the vault header');

    // Extension storage is closed to content scripts.
    assert.deepEqual(await send({ type: 'storageRestricted' }), { restricted: true });

    // Extension pages enforce Trusted Types: HTML strings are refused.
    assert.equal(await dash.evaluate(`(() => { try { document.body.insertAdjacentHTML('beforeend', '<b>x</b>'); return 'allowed'; } catch (e) { return e.name; } })()`), 'TypeError');

    // Save a scheduled recipe and run it in a background tab.
    const saved = await send({ type: 'saveRecipe', recipe: { name: 'List', url: `${base}/list`, rowSelector: 'li.card', fields: [{ name: 'name', selector: '.name' }], nextSelector: 'a.next', maxPages: 3, scheduleMinutes: 15 } });
    assert.equal(saved.error, undefined, saved.error);
    assert.ok((await dash.evaluate(`chrome.alarms.getAll()`)).some(a => a.name === `recipe:${saved.id}` && a.periodInMinutes === 15), 'a schedule alarm exists');
    assert.deepEqual(await send({ type: 'runNow', id: saved.id }), { ok: true });
    const first = await waitFor(`${runs}.then(rs => rs.find(r => r.status !== 'running'))`, 'the tab run');
    assert.equal(first.status, 'success', first.error);
    assert.equal(first.via, 'tab');
    assert.equal(first.rowCount, 6);
    // A second run stores its rows too: random keys mean storage can't spot repeats.
    assert.deepEqual(await send({ type: 'runNow', id: saved.id }), { ok: true });
    await waitFor(`${runs}.then(rs => rs.length === 2 && rs.every(r => r.status === 'success'))`, 'the second run');
    // Stored records are ciphertext under random keys: nothing about the rows shows.
    const stored = await dash.evaluate(records);
    assert.equal(stored.length, 12);
    assert.doesNotMatch(JSON.stringify(stored), /Item \d|observedAt/);
    assert.equal(new Set(stored.map(r => r.key)).size, 12);

    // Unlock in the UI and read the records.
    await dash.evaluate(`location.reload()`);
    await sleep(1000);
    await waitFor(`document.querySelectorAll('#vault input').length === 1`, 'the unlock form');
    await dash.evaluate(`(() => { document.querySelector('#vault input').value = ${JSON.stringify(PASS)}; document.querySelector('#vault button').click(); })()`);
    await waitFor(`document.querySelector('#vault')?.textContent.includes('Vault unlocked')`, 'unlocking');
    await waitFor(`[...document.querySelectorAll('#recipes button')].some(b => b.textContent === 'Records')`, 'the recipe list');
    await dash.evaluate(`[...document.querySelectorAll('#recipes button')].find(b => b.textContent === 'Records').click()`);
    const table = await waitFor(`document.querySelector('#records table')?.innerText`, 'the records table');
    for (let i = 1; i <= 6; i++) assert.match(table, new RegExp(`Item ${i}\\b`));
    // Opening the records removed the second run's repeat copies.
    assert.match(await dash.evaluate(`document.querySelector('#records').innerText`), /6 record\(s\); 6 repeat copies from later runs removed/);
    assert.equal((await dash.evaluate(records)).length, 6);

    // The picker ignores synthetic clicks and accepts a real one.
    const page = await open(`${base}/list`);
    const tabId = await dash.evaluate(`chrome.tabs.query({ url: ${JSON.stringify(`${base}/list`)} }).then(t => t[0].id)`);
    assert.deepEqual(await send({ type: 'startPicker', tabId, mode: 'field', fieldName: 'title', kind: 'text' }), { ok: true });
    await page.evaluate(`document.querySelector('.name').click()`);
    await sleep(400);
    assert.equal((await dash.evaluate(`chrome.storage.local.get('draft').then(d => d.draft?.fields?.length ?? 0)`)), 0, 'synthetic click ignored');
    const box = await page.evaluate(`(() => { const r = document.querySelector('.name').getBoundingClientRect(); return { x: r.x + r.width / 2, y: r.y + r.height / 2 }; })()`);
    for (const type of ['mouseMoved', 'mousePressed', 'mouseReleased']) {
      await call('Input.dispatchMouseEvent', { type, x: box.x, y: box.y, button: 'left', clickCount: 1 }, page.sessionId);
    }
    const draft = await waitFor(`chrome.storage.local.get('draft').then(d => d.draft?.fields?.length ? d.draft : null)`, 'the picked field', 10_000);
    assert.deepEqual(draft.fields, [{ name: 'title', selector: 'span.name', kind: 'text' }]);
    assert.match(draft.lastPick, /\(2 matches\)/);
    assert.equal(await page.evaluate(`document.querySelector('pss-picker')`), null, 'the picker removed itself');

    // Picking a record container on plain-div cards climbs from the clicked text to the card.
    const cardPage = await open(`${base}/cards`);
    const cardTab = await dash.evaluate(`chrome.tabs.query({ url: ${JSON.stringify(`${base}/cards`)} }).then(t => t[0].id)`);
    assert.deepEqual(await send({ type: 'startPicker', tabId: cardTab, mode: 'row', fieldName: '', kind: 'text' }), { ok: true });
    const text = await cardPage.evaluate(`(() => { const r = document.querySelector('.text').getBoundingClientRect(); return { x: r.x + r.width / 2, y: r.y + r.height / 2 }; })()`);
    for (const type of ['mouseMoved', 'mousePressed', 'mouseReleased']) {
      await call('Input.dispatchMouseEvent', { type, x: text.x, y: text.y, button: 'left', clickCount: 1 }, cardPage.sessionId);
    }
    assert.equal(await waitFor(`chrome.storage.local.get('draft').then(d => d.draft?.rowSelector)`, 'the picked container', 10_000), 'div.quote');
    // A Next link without classes of its own is named by its classed parent, not by
    // position (which would point at "Previous" once that appears).
    assert.deepEqual(await send({ type: 'startPicker', tabId: cardTab, mode: 'next', fieldName: '', kind: 'text' }), { ok: true });
    const next = await cardPage.evaluate(`(() => { const r = document.querySelector('li.next a').getBoundingClientRect(); return { x: r.x + r.width / 2, y: r.y + r.height / 2 }; })()`);
    for (const type of ['mouseMoved', 'mousePressed', 'mouseReleased']) {
      await call('Input.dispatchMouseEvent', { type, x: next.x, y: next.y, button: 'left', clickCount: 1 }, cardPage.sessionId);
    }
    assert.equal(await waitFor(`chrome.storage.local.get('draft').then(d => d.draft?.nextSelector)`, 'the picked Next button', 10_000), 'li.next > a');

    // A run through the native host: sealed in the host, stored by the extension.
    const host = await send({ type: 'hostStatus' });
    assert.equal(host.installed, true, host.error);
    const hosted = await send({ type: 'saveRecipe', recipe: { name: 'Hosted', url: `${base}/list?page=2`, rowSelector: 'li.card', fields: [{ name: 'name', selector: '.name' }], nextSelector: 'a.next', maxPages: 2, useNativeHost: true } });
    assert.equal(hosted.error, undefined, hosted.error);
    await send({ type: 'runNow', id: hosted.id });
    const viaHost = await waitFor(`${runs}.then(rs => rs.find(r => r.recipeId === ${JSON.stringify(hosted.id)} && r.status !== 'running'))`, 'the host run', 90_000);
    assert.equal(viaHost.status, 'success', viaHost.error);
    assert.equal(viaHost.via, 'host');
    assert.equal(viaHost.rowCount, 4);
    await dash.evaluate(`[...document.querySelectorAll('#recipes tr')].find(tr => tr.textContent.includes('Hosted')).querySelector('button:nth-of-type(2)').click()`);
    const hostedTable = await waitFor(`document.querySelector('#recordsTitle').textContent.includes('Hosted') && document.querySelector('#records table')?.innerText`, 'the hosted records');
    for (let i = 3; i <= 6; i++) assert.match(hostedTable, new RegExp(`Item ${i}\\b`));
  } finally {
    child.kill();
    await new Promise(r => child.once('exit', r));
    server.close();
    rmSync(work, { recursive: true, force: true });
  }
});
