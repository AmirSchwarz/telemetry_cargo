/** Actual WASM engine exercised in Node; DOM/browser integration is separate. */
import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { createBrowserTelemetry } from '../browser.mjs';
import * as wasmModule from '../wasm/telemetry_wasm.js';

const bytes = await readFile(new URL('../wasm/telemetry_wasm_bg.wasm', import.meta.url));
await wasmModule.default({ module_or_path: bytes });
const sdk = await createBrowserTelemetry({ app_id: 'test.browser', app_version: '1.0.0', endpoint: 'https://example.invalid/v1/telemetry/batches' }, { wasmModule });
assert.equal(sdk.capabilities().binding, 'wasm');
assert.equal(sdk.capabilities().indexeddb, false);
sdk.action('search.submit');
assert.equal(sdk.diagnostics().queue_depth, 0);
sdk.setConsent('usage_aggregate', true);
sdk.setConsent('diagnostics', true);
sdk.action('search.submit');
sdk.captureError('error.test', 'synthetic-token /private/document https://example.com');
assert.equal(sdk.diagnostics().queue_depth, 2);
assert.throws(() => sdk.event('action', { id: 'search.submit', result: 'success', query: 'secret' }));
assert.throws(() => sdk.setConsent('usage_session', true));

const originalFetch = globalThis.fetch;
const bodies = [];
globalThis.fetch = async (_url, options) => {
  bodies.push(options.body);
  return new Response('', { status: bodies.length === 1 ? 503 : 200 });
};
try {
  await sdk.flush(3000);
} finally {
  globalThis.fetch = originalFetch;
}
assert.equal(bodies.length, 2);
assert.equal(bodies[0], bodies[1]);
assert.equal(bodies[0].includes('synthetic-token'), false);
assert.equal(bodies[0].includes('/private/document'), false);
assert.equal(JSON.parse(bodies[0]).events[1].properties.message, '[redacted]');
assert.equal(sdk.diagnostics().queue_depth, 0);
sdk.action('search.submit');
sdk.setConsent('usage_aggregate', false);
assert.equal(sdk.diagnostics().queue_depth, 0);
await sdk.shutdown();

const mutable = { app_id: 'test.mutation', app_version: '1.0.0', endpoint: 'https://example.invalid/v1/telemetry/batches', ingestion_token: 'public-original', consent: { usage_aggregate: false } };
let resolveInit;
const initializing = createBrowserTelemetry(mutable, { wasmModule: {
  default: () => new Promise(resolve => { resolveInit = resolve; }),
  WasmTelemetry: wasmModule.WasmTelemetry,
} });
mutable.endpoint = 'http://insecure.invalid/v1/telemetry/batches';
mutable.ingestion_token = 'changed';
mutable.consent.usage_aggregate = true;
resolveInit();
const snapshotSdk = await initializing;
snapshotSdk.action('search.submit');
assert.equal(snapshotSdk.diagnostics().queue_depth, 0);
snapshotSdk.setConsent('usage_aggregate', true);
snapshotSdk.action('search.submit');
globalThis.fetch = async (url, options) => {
  assert.equal(url, 'https://example.invalid/v1/telemetry/batches');
  assert.equal(options.headers.Authorization, 'Bearer public-original');
  return new Response('', { status: 200 });
};
try { await snapshotSdk.flush(); } finally { globalThis.fetch = originalFetch; }
snapshotSdk.action('search.submit');
globalThis.fetch = async () => {
  await new Promise(resolve => setTimeout(resolve, 40));
  return new Response('', { status: 200 });
};
let originalFinished = false;
const originalFlush = snapshotSdk.flush(1000).then(() => { originalFinished = true; });
try {
  await assert.rejects(snapshotSdk.flush(1), /deadline exceeded/);
  assert.equal(originalFinished, false);
  await originalFlush;
} finally { globalThis.fetch = originalFetch; }
const closing = snapshotSdk.shutdown();
assert.throws(() => snapshotSdk.action('search.submit'), /closed/);
await closing;
await snapshotSdk.shutdown();

const failed = await createBrowserTelemetry({ app_id: 'test.offline', app_version: '1.0.0', consent: { usage_aggregate: true } }, { wasmModule });
failed.action('search.submit');
await assert.rejects(failed.shutdown());
assert.throws(() => failed.action('search.submit'));

const raw = new wasmModule.WasmTelemetry(JSON.stringify({ app_id: 'test.strict', app_version: '1.0.0' }));
assert.throws(() => raw.command('{"op":"diagnostics","extra":true}', Date.now()));
raw.free();
console.log('Browser WASM smoke passed (Node host, mocked HTTPS fetch)');
