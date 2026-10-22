// Exercises the compiled WebAssembly core through the same JS bindings the extension uses.
import test from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import init, * as core from '../extension/build/core/pocket_scraper_core_wasm.js';

await init({ module_or_path: readFileSync(new URL('../extension/build/core/pocket_scraper_core_wasm_bg.wasm', import.meta.url)) });

const recipe = { name: 'Jobs', url: 'https://example.org/jobs', fields: [{ name: 'title', selector: '.t' }], workflow: null };
const PASS = 'correct horse battery staple';

test('recipes are validated and normalised by the core', () => {
  const out = JSON.parse(core.validateRecipe(JSON.stringify(recipe)));
  assert.equal(out.id.length, 32);
  assert.equal(out.maxPages, 1);
  assert.equal(core.recipeOrigin(JSON.stringify(recipe)), 'https://example.org');
  assert.throws(() => core.validateRecipe(JSON.stringify({ ...recipe, url: 'file:///etc/passwd' })), /http and https/);
  assert.throws(() => core.validateRecipe(JSON.stringify({ ...recipe, extra: 1 })), /unknown field/);
});

test('the planner drives a run over JSON and refuses malformed observations', () => {
  const id = JSON.parse(core.validateRecipe(JSON.stringify(recipe))).id;
  const planner = new core.Planner(JSON.stringify({ ...recipe, id }), 60_000, 5_000);
  let action = JSON.parse(planner.start(0));
  assert.deepEqual(action, { type: 'navigate', url: 'https://example.org/jobs' });
  action = JSON.parse(planner.observe(JSON.stringify({ nowMs: 10, event: { type: 'navigated', status: 200 } })));
  assert.deepEqual(action, { type: 'wait', ms: 400 });
  action = JSON.parse(planner.observe(JSON.stringify({ nowMs: 420, event: { type: 'waited' } })));
  assert.deepEqual(action, { type: 'read' });
  const output = { pageUrl: 'https://example.org/jobs', rows: [{ pageUrl: 'https://example.org/jobs', fields: { title: 'A' } }], warnings: [], emptyFields: [] };
  action = JSON.parse(planner.observe(JSON.stringify({ nowMs: 430, event: { type: 'read', url: output.pageUrl, status: 200, output } })));
  assert.equal(action.type, 'finish');
  assert.deepEqual(action.rows, output.rows);
  planner.free();

  const bad = new core.Planner(JSON.stringify({ ...recipe, id }), 60_000, 5_000);
  bad.start(0);
  assert.equal(JSON.parse(bad.observe('{"nowMs":1,"event":{"type":"navigated","status":200,"evil":1}}')).type, 'fail');
  bad.free();
});

test('rows sealed with only the public key open after unlocking, and tampering is caught', () => {
  const header = core.createVault(PASS);
  assert.doesNotMatch(header, /secret"?:\s*"[A-Za-z0-9+/]{40,}/, 'no raw secret in the header');
  const rows = [{ pageUrl: 'https://example.org/', fields: { title: '=HYPERLINK("x")' } }, { pageUrl: 'https://example.org/', fields: { title: 'B' } }];
  const sealed = JSON.parse(core.sealRows(header, 'recipe-1', 'run-1', '2026-10-22T12:00:00.000Z', JSON.stringify(rows)));
  assert.equal(sealed.length, 2);
  assert.doesNotMatch(JSON.stringify(sealed), /HYPERLINK/);

  assert.throws(() => core.UnlockedVault.unlock(header, 'the wrong passphrase'), /Wrong passphrase/);
  const vault = core.UnlockedVault.unlock(header, PASS);
  const plain = JSON.parse(vault.open('recipe-1', sealed[0].key, sealed[0].sealed));
  assert.equal(plain.fields.title, '=HYPERLINK("x")');
  assert.equal(plain.runId, 'run-1');
  assert.throws(() => vault.open('recipe-2', sealed[0].key, sealed[0].sealed), /could not be opened/);
  const flipped = Buffer.from(sealed[1].sealed, 'base64');
  flipped[flipped.length - 1] ^= 1;
  assert.throws(() => vault.open('recipe-1', sealed[1].key, flipped.toString('base64')), /could not be opened/);

  const csv = core.toCsv(JSON.stringify(['title']), JSON.stringify([plain]));
  assert.match(csv, /"\t=HYPERLINK\(""x""\)"/);
  vault.free();

  const changed = core.changePassphrase(header, PASS, 'another long passphrase');
  const reopened = core.UnlockedVault.unlock(changed, 'another long passphrase');
  assert.equal(JSON.parse(reopened.open('recipe-1', sealed[1].key, sealed[1].sealed)).fields.title, 'B');
  reopened.free();
});

test('records from the native host are checked before storage', () => {
  assert.throws(() => core.checkSealedRecords(JSON.stringify([{ key: 'not-hex', sealed: 'AA==' }])), /malformed/);
  assert.throws(() => core.checkSealedRecords(JSON.stringify([{ key: 'a'.repeat(64), sealed: 'AA==', extra: 1 }])), /malformed/);
  assert.equal(JSON.parse(core.checkSealedRecords(JSON.stringify([{ key: 'a'.repeat(64), sealed: 'AAEC' }]))).length, 1);
});
