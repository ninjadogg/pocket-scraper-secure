// End to end: the built extension in Chrome for Testing (which can load unpacked
// extensions), driven over a debugging pipe. Covers the vault UI, tab runs, sealed
// storage, CSP, the picker, a run through the real native host, and Claude access
// through the MCP server.
// Skips when no Chrome for Testing is found (PSS_E2E_BROWSER overrides the path),
// unless PSS_E2E_REQUIRED is set (CI).
import test from 'node:test';
import assert from 'node:assert/strict';
import { execFile, spawn } from 'node:child_process';
import { promisify } from 'node:util';
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
  const cards = '<title>Cards</title>' + [1, 2, 3].map(i => `<div class="quote"><span class="text">Quote ${i}</span><span>by <small class="author">Author ${i}</small></span></div>`).join('')
    + '<nav><ul class="pager"><li class="previous"><a href="#">Previous</a></li><li class="next"><a href="#">Next</a></li></ul></nav>';
  const server = createServer((req, res) => {
    const url = new URL(req.url, 'http://x');
    const n = Number(url.searchParams.get('page') || 1);
    res.writeHead(200, { 'Content-Type': 'text/html; charset=utf-8' });
    // A hostile page: one link field holds a javascript: URL.
    const links = '<title>Links</title><ul><li class="l"><a href="javascript:alert(1)">Bad</a></li><li class="l"><a href="https://example.org/ok">Good</a></li></ul>';
    res.end(url.pathname === '/cards' ? cards : url.pathname === '/links' ? links : list(n));
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

test('the extension collects, seals, shows and protects records end to end', { timeout: 480_000 }, async t => {
  const browserPath = findBrowser();
  // CI sets PSS_E2E_REQUIRED: there, a missing browser is a failure, not a skip.
  if (!browserPath && process.env.PSS_E2E_REQUIRED) throw new Error('PSS_E2E_REQUIRED is set but no Chrome for Testing was found (PSS_E2E_BROWSER)');
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
  ], { stdio: ['ignore', 'ignore', 'ignore', 'pipe', 'pipe'], env: { ...process.env, PSS_DATA_DIR: join(work, 'host-data'), PSS_BROWSER_PATH: browserPath } });
  const { call } = cdp(child);
  let s3;
  try {
    // The manifest key pins the extension ID the native host allows.
    let worker;
    for (let i = 0; i < 50 && !worker; i++) {
      worker = (await call('Target.getTargets')).targetInfos.find(t => t.type === 'service_worker' && t.url === `chrome-extension://${EXTENSION_ID}/background.js`);
      if (!worker) await sleep(100);
    }
    assert.ok(worker, 'the service worker started under the pinned extension ID');
    // Installing opens the dashboard (where the vault is created) by itself.
    const dashboards = async () => (await call('Target.getTargets')).targetInfos.filter(t => t.type === 'page' && t.url.startsWith(`chrome-extension://${EXTENSION_ID}/dashboard.html`)).length;
    for (let i = 0; i < 100 && (await dashboards()) < 1; i++) await sleep(100);
    assert.equal(await dashboards(), 1, 'installing opened the dashboard');

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

    // The dashboard tour starts by itself once (in the dashboard that opened first)
    // and can be replayed; it starts with the vault.
    await waitFor(`chrome.storage.local.get('toursSeen').then(t => t.toursSeen?.dashboard === true)`, 'the tour shown once');
    if (!(await dash.evaluate(`document.querySelector('.tour') !== null`))) await dash.evaluate(`document.querySelector('#tour').click()`);
    await waitFor(`document.querySelector('.tour-card h3')?.textContent === 'Start here: create your vault'`, 'the dashboard tour');
    await dash.evaluate(`document.querySelector('.tour-skip').click()`);
    assert.equal(await dash.evaluate(`document.querySelector('.tour')`), null);

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
    assert.equal(await dash.evaluate(`document.querySelector('.tour')`), null, 'the tour doesn’t come back once seen');
    await dash.evaluate(`(() => { document.querySelector('#vault input').value = ${JSON.stringify(PASS)}; document.querySelector('#vault button').click(); })()`);
    await waitFor(`document.querySelector('#vault')?.textContent.includes('Vault unlocked')`, 'unlocking');
    await waitFor(`[...document.querySelectorAll('#recipes button')].some(b => b.textContent === 'Records')`, 'the recipe list');
    await dash.evaluate(`[...document.querySelectorAll('#recipes button')].find(b => b.textContent === 'Records').click()`);
    const table = await waitFor(`document.querySelector('#records table')?.innerText`, 'the records table');
    for (let i = 1; i <= 6; i++) assert.match(table, new RegExp(`Item ${i}\\b`));
    // Opening the records removed the second run's repeat copies.
    assert.match(await dash.evaluate(`document.querySelector('#records').innerText`), /6 records; 6 repeat copies from later runs removed/);
    assert.equal((await dash.evaluate(records)).length, 6);

    // Teach once, run anywhere: export a signed bundle, run it in the runner binary,
    // enrol the runner and import its signed, sealed batch.
    const downloads = join(work, 'downloads');
    mkdirSync(downloads);
    await call('Browser.setDownloadBehavior', { behavior: 'allow', downloadPath: downloads });
    const fingerprint = await waitFor(`document.querySelector('#vaultSettings code')?.textContent`, 'the vault fingerprint');
    await dash.evaluate(`[...document.querySelectorAll('#recipes button')].find(b => b.textContent === 'Export for runner').click()`);
    const bundleFile = join(downloads, 'List.pssbundle');
    for (let i = 0; i < 50 && !existsSync(bundleFile); i++) await sleep(100);
    const runnerEnv = { ...process.env, PSS_DATA_DIR: join(work, 'runner-data'), PSS_BROWSER_PATH: browserPath };
    // Asynchronous: the test site is served from this process, so it must keep running.
    const runner = async (...args) => (await promisify(execFile)(join(root, 'target', 'debug', 'pocket-scraper-runner'), args, { env: runnerEnv, encoding: 'utf8' })).stdout;
    const runnerKey = (await runner('id')).match(/key:\s+(\S+)/)[1];
    assert.match(await runner('add', bundleFile, '--trust', fingerprint), /Added "List"/);
    assert.match(await runner('run', saved.id), /Success: 6 rows/);
    const batchFile = readdirSync(join(work, 'runner-data', 'runner', 'outbox')).map(f => join(work, 'runner-data', 'runner', 'outbox', f))[0];
    const tamperedFile = join(work, 'tampered.pssbatch');
    writeFileSync(tamperedFile, readFileSync(batchFile, 'utf8').replace(/"signature": "./, '"signature": "A'));
    const { root: { nodeId: docId } } = await call('DOM.getDocument', {}, dash.sessionId);
    const { nodeId: inputId } = await call('DOM.querySelector', { nodeId: docId, selector: '#importBatches' }, dash.sessionId);
    const importFiles = async files => {
      await dash.evaluate(`document.querySelector('#importMessage').textContent = ''`);
      await call('DOM.setFileInputFiles', { nodeId: inputId, files }, dash.sessionId);
      return waitFor(`document.querySelector('#importMessage').textContent`, 'the import result');
    };
    assert.match(await importFiles([batchFile]), /refused \(The batch comes from a runner that isn't enrolled\)/);
    await dash.evaluate(`(() => { const [name, key] = document.querySelectorAll('#runners input'); name.value = 'test-runner'; key.value = ${JSON.stringify(runnerKey)}; key.dispatchEvent(new Event('input')); [...document.querySelectorAll('#runners button')].find(b => b.textContent === 'Add runner').click(); })()`);
    await waitFor(`[...document.querySelectorAll('#runners span')].some(s => s.textContent === 'test-runner')`, 'the enrolled runner');
    assert.match(await importFiles([batchFile]), /success, 6 rows from test-runner/);
    assert.match(await importFiles([batchFile, tamperedFile]), /already imported.*tampered\.pssbatch: refused/);
    // Through a bucket: the runner uploads after its run, the desktop host collects.
    const bucket = new Map();
    s3 = createServer((req, res) => {
      const url = new URL(req.url, 'http://x');
      const chunks = [];
      req.on('data', c => chunks.push(c));
      req.on('end', () => {
        const key = decodeURIComponent(url.pathname.replace(/^\/bucket\/?/, ''));
        if (req.method === 'PUT') { bucket.set(key, Buffer.concat(chunks)); res.end(); }
        else if (!key) {
          const prefix = url.searchParams.get('prefix') ?? '';
          const keys = [...bucket.keys()].sort().filter(k => k.startsWith(prefix));
          const body = url.searchParams.get('delimiter') === '/'
            ? [...new Set(keys.map(k => k.slice(prefix.length)).filter(r => r.includes('/')).map(r => prefix + r.slice(0, r.indexOf('/') + 1)))].map(f => `<CommonPrefixes><Prefix>${f}</Prefix></CommonPrefixes>`).join('')
            : keys.filter(k => k > (url.searchParams.get('start-after') ?? '')).slice(0, Number(url.searchParams.get('max-keys') ?? 1000)).map(k => `<Contents><Key>${k}</Key></Contents>`).join('');
          res.end(`<ListBucketResult><IsTruncated>false</IsTruncated>${body}</ListBucketResult>`);
        }
        else if (bucket.has(key)) res.end(bucket.get(key));
        else { res.statusCode = 404; res.end(); }
      });
    });
    await new Promise(r => s3.listen(0, '127.0.0.1', r));
    const s3Args = ['--endpoint', `http://127.0.0.1:${s3.address().port}`, '--bucket', 'bucket', '--access-key-id', 'AK'];
    const secretEnv = { ...runnerEnv, PSS_S3_SECRET_ACCESS_KEY: 'secret' };
    await promisify(execFile)(join(root, 'target', 'debug', 'pocket-scraper-runner'), ['upload', 'configure', ...s3Args], { env: secretEnv });
    await promisify(execFile)(hostBinary, ['collector', 'configure', ...s3Args], { env: { ...secretEnv, PSS_DATA_DIR: join(work, 'host-data') } });
    assert.match(await runner('run', saved.id), /Uploaded .*\.pssbatch/);
    // The earlier run's batch was still in the outbox, so both go up.
    assert.equal(bucket.size, 2);
    await dash.evaluate(`document.querySelector('#importMessage').textContent = ''`);
    await dash.evaluate(`document.querySelector('#collect').click()`);
    assert.match(await waitFor(`document.querySelector('#importMessage').textContent.includes('rows') && document.querySelector('#importMessage').textContent`, 'the collected batch'), /already imported.*success, 6 rows from test-runner|success, 6 rows from test-runner.*already imported/);
    await dash.evaluate(`document.querySelector('#importMessage').textContent = ''; document.querySelector('#collect').click()`);
    assert.equal(await waitFor(`document.querySelector('#importMessage').textContent.startsWith('No new') && document.querySelector('#importMessage').textContent`, 'nothing new'), 'No new results in the bucket.');

    const runnerRun = await waitFor(`${runs}.then(rs => rs.find(r => r.via === 'runner'))`, 'the imported run');
    assert.equal(runnerRun.runner, 'test-runner');
    // Same rows as the tab runs: the open records view refreshes after the import and
    // removes the repeat copies, leaving the 6 distinct records.
    await waitFor(`${records}.then(r => r.length === 6)`, 'compacted records');
    assert.match(await dash.evaluate(`document.querySelector('#records p.small').textContent`), /^6 records/);

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
    // The picker confirms the pick briefly, then removes itself.
    for (let i = 0; i < 40 && await page.evaluate(`document.querySelector('pss-picker') !== null`); i++) await sleep(100);
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
    // A field inside the card is named by its own class, not by a tag that also
    // matches the card's other span.
    assert.deepEqual(await send({ type: 'startPicker', tabId: cardTab, mode: 'field', fieldName: 'quote', kind: 'text' }), { ok: true });
    for (const type of ['mouseMoved', 'mousePressed', 'mouseReleased']) {
      await call('Input.dispatchMouseEvent', { type, x: text.x, y: text.y, button: 'left', clickCount: 1 }, cardPage.sessionId);
    }
    assert.equal(await waitFor(`chrome.storage.local.get('draft').then(d => d.draft?.fields?.find(f => f.name === 'quote')?.selector)`, 'the picked quote field', 10_000), 'span.text');
    // A Next link without classes of its own is named by its classed parent, not by
    // position (which would point at "Previous" once that appears).
    assert.deepEqual(await send({ type: 'startPicker', tabId: cardTab, mode: 'next', fieldName: '', kind: 'text' }), { ok: true });
    // Plain text isn't a Next button: refused, and the picker keeps waiting.
    for (const type of ['mouseMoved', 'mousePressed', 'mouseReleased']) {
      await call('Input.dispatchMouseEvent', { type, x: text.x, y: text.y, button: 'left', clickCount: 1 }, cardPage.sessionId);
    }
    await sleep(400);
    assert.equal(await dash.evaluate(`chrome.storage.local.get('draft').then(d => d.draft?.nextSelector ?? '')`), '');
    assert.ok(await cardPage.evaluate(`document.querySelector('pss-picker') !== null`), 'the picker is still waiting');
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
    await dash.evaluate(`[...[...document.querySelectorAll('#recipes tr')].find(tr => tr.textContent.includes('Hosted')).querySelectorAll('button')].find(b => b.textContent === 'Records').click()`);
    const hostedTable = await waitFor(`document.querySelector('#recordsTitle').textContent.includes('Hosted') && document.querySelector('#records table')?.innerText`, 'the hosted records');
    for (let i = 3; i <= 6; i++) assert.match(hostedTable, new RegExp(`Item ${i}\\b`));

    // The records view: only http(s) values become links; search and sorting work.
    const linked = await send({ type: 'saveRecipe', recipe: { name: 'Links', url: `${base}/links`, rowSelector: 'li.l', fields: [{ name: 'label', selector: 'a' }, { name: 'target', selector: 'a', kind: 'href' }] } });
    assert.equal(linked.error, undefined, linked.error);
    await send({ type: 'runNow', id: linked.id });
    await waitFor(`${runs}.then(rs => rs.some(r => r.recipeId === ${JSON.stringify(linked.id)} && r.status === 'success'))`, 'the links run');
    await dash.evaluate(`[...[...document.querySelectorAll('#recipes tr')].find(tr => tr.textContent.includes('Links')).querySelectorAll('button')].find(b => b.textContent === 'Records').click()`);
    await waitFor(`document.querySelector('#recordsTitle').textContent.includes('Links') && document.querySelector('#records table')`, 'the links records');
    assert.equal(await dash.evaluate(`location.hash`), '#records');
    assert.equal(await dash.evaluate(`document.querySelectorAll('#records a[href^="javascript"]').length`), 0, 'no javascript: links');
    assert.match(await dash.evaluate(`document.querySelector('#records table').innerText`), /javascript:alert\(1\)/, 'shown as text');
    assert.equal(await dash.evaluate(`document.querySelector('#records a[href="https://example.org/ok"]')?.rel`), 'noopener noreferrer');
    await dash.evaluate(`(() => { const s = document.querySelector('#records input[type=search]'); s.value = 'good'; s.dispatchEvent(new Event('input')); })()`);
    await waitFor(`document.querySelectorAll('#records table tr').length === 2`, 'the search filter');
    await dash.evaluate(`(() => { const s = document.querySelector('#records input[type=search]'); s.value = ''; s.dispatchEvent(new Event('input')); })()`);
    await waitFor(`document.querySelectorAll('#records table tr').length === 3`, 'the cleared search');
    await dash.evaluate(`[...document.querySelectorAll('#records th button')].find(b => b.textContent.startsWith('label')).click()`);
    assert.deepEqual(await dash.evaluate(`[...document.querySelectorAll('#records table tr')].slice(1).map(tr => tr.children[1].textContent)`), ['Bad', 'Good']);
    await dash.evaluate(`[...document.querySelectorAll('#records th button')].find(b => b.textContent.startsWith('label')).click()`);
    assert.deepEqual(await dash.evaluate(`[...document.querySelectorAll('#records table tr')].slice(1).map(tr => tr.children[1].textContent)`), ['Good', 'Bad']);
    assert.equal(await dash.evaluate(`document.querySelector('#recordsRecipe').value`), linked.id);

    // Claude access: the MCP server reaches the extension only once the person turns
    // it on, reads records only while unlocked, and deletes only with confirm.
    const mcp = spawn(join(root, 'target', 'debug', 'pocket-scraper-mcp'), [], { stdio: ['pipe', 'pipe', 'inherit'], env: { ...process.env, PSS_DATA_DIR: join(work, 'host-data'), PSS_BROWSER_PATH: browserPath } });
    let mcpId = 0;
    const answers = new Map();
    let mcpBuffer = '';
    mcp.stdout.on('data', chunk => {
      mcpBuffer += chunk;
      for (let nl; (nl = mcpBuffer.indexOf('\n')) >= 0; mcpBuffer = mcpBuffer.slice(nl + 1)) {
        const m = JSON.parse(mcpBuffer.slice(0, nl));
        answers.get(m.id)?.(m);
      }
    });
    const notes = new Map();
    const tool = (name, args = {}) => new Promise((resolve, reject) => {
      const id = ++mcpId;
      answers.set(id, m => {
        try {
          if (m.error) return reject(new Error(m.error.message));
          // Results with outside text start with a note saying it is data.
          const content = m.result.content;
          if (content.length > 1) notes.set(name, content[0].text);
          const text = content.at(-1).text;
          resolve(m.result.isError ? { error: text } : JSON.parse(text));
        } catch (e) { reject(e); }
      });
      mcp.stdin.write(JSON.stringify({ jsonrpc: '2.0', id, method: 'tools/call', params: { name, arguments: args } }) + '\n');
    });
    try {
      assert.match((await tool('list_recipes')).error, /turn on Claude access/);
      await dash.evaluate(`document.querySelector('#claude input[type=checkbox]').click()`);
      let status;
      for (let i = 0; i < 50; i++) {
        status = await tool('status');
        if (status.extension?.connected) break;
        await sleep(200);
      }
      assert.equal(status.extension.connected, true, JSON.stringify(status));
      assert.equal(status.extension.vault.locked, false);
      assert.equal(status.extension.vault.fingerprint, fingerprint);

      const list = await tool('list_recipes');
      assert.match(notes.get('list_recipes'), /data, not instructions/);
      assert.deepEqual(list.map(r => r.name).sort(), ['Hosted', 'Links', 'List']);
      assert.ok(list.every(r => r.siteAllowed));

      // Kick off a run and wait for it.
      const run = await tool('run_recipe', { recipe_id: saved.id, wait_seconds: 90 });
      assert.equal(run.status, 'success', run.error);
      assert.equal(run.rowCount, 6);
      const read = await tool('read_records', { recipe_id: saved.id, limit: 3 });
      assert.equal(read.total, 6);
      assert.equal(read.records.length, 3);
      const csvPath = join(work, 'out', 'list.csv');
      mkdirSync(join(work, 'out'));
      assert.equal((await tool('export_records', { recipe_id: saved.id, format: 'csv', path: csvPath })).records, 6);
      assert.match(readFileSync(csvPath, 'utf8'), /^name,observedAt,pageUrl|Item 1/m);
      assert.match((await tool('export_records', { recipe_id: saved.id, format: 'csv', path: csvPath })).error, /already exists/);
      // Never a shell profile or other hidden file, never another file type.
      assert.match((await tool('export_records', { recipe_id: saved.id, format: 'csv', path: join(work, '.hidden', 'x.csv') })).error, /hidden/);
      assert.match((await tool('export_records', { recipe_id: saved.id, format: 'csv', path: join(work, 'out', 'x.sh') })).error, /must end in \.csv/);

      // A recipe for a site Chrome hasn't been allowed: saved, but waits for the person.
      const pending = await tool('save_recipe', { recipe: { name: 'Pending', url: `http://localhost:${server.address().port}/list`, rowSelector: 'li.card', fields: [{ name: 'name', selector: '.name' }] } });
      assert.equal(pending.siteAllowed, false);
      assert.equal(pending.waitingForReview, true);
      await dash.evaluate(`location.reload()`);
      await waitFor(`[...document.querySelectorAll('#recipes tr')].some(tr => tr.textContent.includes('Pending') && [...tr.querySelectorAll('button')].some(b => b.textContent === 'Allow access'))`, 'the Allow access button');

      // Even for an allowed site, a recipe Claude made runs only after the person reviews it.
      const claudeMade = await tool('save_recipe', { recipe: { name: 'Claude list', url: `${base}/list`, rowSelector: 'li.card', fields: [{ name: 'name', selector: '.name' }] } });
      assert.equal(claudeMade.siteAllowed, true);
      assert.equal(claudeMade.waitingForReview, true);
      assert.match((await tool('run_recipe', { recipe_id: claudeMade.recipe.id })).error, /review/);
      await waitFor(`document.getElementById('review-${claudeMade.recipe.id}') !== null`, 'the Review button');
      await dash.evaluate(`document.getElementById('review-${claudeMade.recipe.id}').click()`);
      await waitFor(`document.querySelector('.review-row')?.textContent.includes('Starts at')`, 'the review panel');
      await dash.evaluate(`document.getElementById('approve-${claudeMade.recipe.id}').click()`);
      await waitFor(`chrome.storage.local.get('recipes').then(r => !r.recipes[${JSON.stringify(claudeMade.recipe.id)}].review)`, 'the approval');
      assert.equal((await tool('run_recipe', { recipe_id: claudeMade.recipe.id, wait_seconds: 90 })).status, 'success');
      // A Claude edit that only renames it needs no new review; a new address does.
      assert.equal((await tool('save_recipe', { recipe: { ...claudeMade.recipe, name: 'Claude list 2' } })).waitingForReview, false);
      assert.equal((await tool('save_recipe', { recipe: { ...claudeMade.recipe, url: `${base}/list?page=2` } })).waitingForReview, true);

      // Settings: the auto-lock can be shortened, not lengthened; out-of-range values are refused.
      assert.equal((await tool('update_settings', { auto_lock_minutes: 5 })).autoLockMinutes, 5);
      assert.match((await tool('update_settings', { auto_lock_minutes: 30 })).error, /Only the person/);
      assert.match((await tool('update_settings', { run_timeout_ms: 1 })).error, /out of range/);
      assert.equal((await tool('get_settings')).claudeAccess, true);

      // Teach → run on this machine's runner → enrol it → import its batch, all via Claude.
      // (The reload above locked the vault: bundles need it unlocked.)
      assert.match((await tool('export_bundle', { recipe_id: saved.id, add_to_this_runner: true })).error, /unlock/);
      await waitFor(`document.querySelectorAll('#vault input').length === 1`, 'the unlock form');
      await dash.evaluate(`(() => { document.querySelector('#vault input').value = ${JSON.stringify(PASS)}; document.querySelector('#vault button').click(); })()`);
      await waitFor(`document.querySelector('#vault')?.textContent.includes('Vault unlocked')`, 'unlocking');
      // A site the person hasn't allowed can't be signed for a runner either.
      assert.match((await tool('export_bundle', { recipe_id: pending.recipe.id, add_to_this_runner: true })).error, /review/);
      // Deletes need confirm.
      assert.match((await tool('delete_recipe', { recipe_id: pending.recipe.id })).error, /confirm: true/);
      assert.deepEqual(await tool('delete_recipe', { recipe_id: pending.recipe.id, confirm: true }), { deleted: pending.recipe.id, recordsDeleted: false });
      const bundle = await tool('export_bundle', { recipe_id: saved.id, add_to_this_runner: true });
      assert.equal(bundle.addedToRunner.recipeId, saved.id, JSON.stringify(bundle));
      const here = await tool('runner_status');
      assert.equal(here.recipes.length, 1);
      const ran = await tool('runner_run', { recipe_id: saved.id });
      assert.equal(ran.status, 'success', ran.error);
      assert.match((await tool('import_batches', { paths: [join(work, 'host-data', 'runner', 'outbox')] })).results.join(' '), /isn't enrolled/);
      // Enrolled by Claude: pending until the person approves it.
      const enrolled = await tool('enroll_runner', { name: 'this-mac', key: here.key });
      assert.equal(enrolled.fingerprint, here.fingerprint);
      assert.equal(enrolled.pending, true);
      assert.match((await tool('import_batches', { paths: [join(work, 'host-data', 'runner', 'outbox')] })).results.join(' '), /waiting for approval/);
      await waitFor(`[...document.querySelectorAll('#runners button')].some(b => b.textContent === 'Approve')`, 'the Approve button');
      await dash.evaluate(`[...document.querySelectorAll('#runners button')].find(b => b.textContent === 'Approve').click()`);
      // A second click confirms, next to the fingerprint to compare.
      assert.ok(await dash.evaluate(`document.querySelector('#runnerList').textContent.includes(${JSON.stringify(here.fingerprint)})`));
      await waitFor(`[...document.querySelectorAll('#runners button')].some(b => b.textContent === 'Yes, approve')`, 'the confirmation');
      await dash.evaluate(`[...document.querySelectorAll('#runners button')].find(b => b.textContent === 'Yes, approve').click()`);
      await waitFor(`chrome.storage.local.get('runners').then(r => r.runners.every(x => !x.pending))`, 'the approval');
      assert.match((await tool('import_batches', { paths: [join(work, 'host-data', 'runner', 'outbox')] })).results.join(' '), /success, 6 rows from this-mac/);

      // Bucket settings for the collector, with the secret from a file; then collect.
      const secretFile = join(work, 'secret.txt');
      writeFileSync(secretFile, 'secret\n');
      const bucketArgs = { role: 'collector', endpoint: `http://127.0.0.1:${s3.address().port}`, bucket: 'bucket', access_key_id: 'AK', secret_file: secretFile };
      // Claude only proposes: nothing changes until the person approves it.
      const configured = await tool('configure_bucket', bucketArgs);
      assert.equal(configured.proposed.configured, true);
      assert.equal(configured.replaces, true);
      assert.doesNotMatch(JSON.stringify(configured), /secret"/);
      assert.equal((await tool('status')).bucketsWaitingForApproval[0].role, 'collector');
      // Too long a folder prefix (it could carry data off the machine) is refused.
      assert.match((await tool('configure_bucket', { ...bucketArgs, prefix: 'x'.repeat(65) })).error, /64/);
      await dash.evaluate(`document.querySelector('#tab-settings').click()`);
      await waitFor(`document.getElementById('bucket-approve-collector') !== null`, 'the bucket approval', 20_000);
      assert.match(await dash.evaluate(`document.querySelector('#buckets').textContent`), /Replaces current settings/);
      await dash.evaluate(`document.getElementById('bucket-approve-collector').click()`);
      await waitFor(`document.getElementById('bucket-approve-collector') === null`, 'the approved bucket');
      assert.deepEqual((await tool('status')).bucketsWaitingForApproval, []);
      assert.equal((await tool('test_bucket')).ok, true);
      assert.deepEqual((await tool('collect_from_bucket')).results, []);

      assert.match((await tool('remove_runner', { runner: 'this-mac' })).error, /confirm: true/);
      assert.deepEqual(await tool('remove_runner', { runner: 'this-mac', confirm: true }), { removed: 1 });

      // Locked: records can't be read.
      await dash.evaluate(`[...document.querySelectorAll('#vault button')].find(b => b.textContent === 'Lock now').click()`);
      assert.match((await tool('read_records', { recipe_id: saved.id })).error, /unlock/);

      // Turning Claude access off closes the bridge.
      await dash.evaluate(`document.querySelector('#claude input[type=checkbox]').click()`);
      for (let i = 0; i < 50 && !(await tool('list_recipes')).error; i++) await sleep(200);
      assert.match((await tool('list_recipes')).error, /turn on Claude access/);
    } finally {
      mcp.stdin.end();
      await new Promise(r => mcp.once('exit', r));
    }
  } finally {
    child.kill();
    await new Promise(r => child.once('exit', r));
    server.close();
    s3?.close();
    rmSync(work, { recursive: true, force: true });
  }
});
