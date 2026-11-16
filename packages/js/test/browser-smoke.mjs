/** Actual WASM engine exercised in Node; DOM/browser integration is separate. */
import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { BrowserTelemetry, createBrowserTelemetry } from '../browser.mjs';
import * as wasmModule from '../wasm/telemetry_wasm.js';

const bytes = await readFile(new URL('../wasm/telemetry_wasm_bg.wasm', import.meta.url));
await wasmModule.default({ module_or_path: bytes });
const originalFetch = globalThis.fetch;
const create = (config = {}) => createBrowserTelemetry({ app_id: 'test.browser', app_version: '1.0.0', endpoint: 'https://example.invalid/otlp', ...config }, { wasmModule });
const ok = () => new Response('{}', { status: 200, headers: { 'Content-Type': 'application/json' } });
const withFetch = async (fetch, work) => {
  globalThis.fetch = fetch;
  try { return await work(); } finally { globalThis.fetch = originalFetch; }
};
const sdk = await create({ otlp_headers: { authorization: 'public-clickstack-key' } });
assert.equal(sdk.capabilities().binding, 'wasm');
assert.equal(sdk.capabilities().indexeddb, false);
assert.equal(sdk.capabilities().export_protocol, 'otlp_http_json');
sdk.action('search.submit');
assert.equal(sdk.diagnostics().queue_depth, 0);
for (const category of ['usage_aggregate', 'diagnostics', 'performance']) sdk.setConsent(category, true);
sdk.action('search.submit');
sdk.captureError('error.test', 'synthetic-token /private/document https://example.com');
sdk.increment('documents.imported', 3);
sdk.span('document.ocr')();
assert.equal(sdk.diagnostics().queue_depth, 4);
assert.throws(() => sdk.event('action', { id: 'search.submit', result: 'success', query: 'secret' }));
assert.throws(() => sdk.setConsent('usage_session', true));
const sent = [];
let metricAttempts = 0;
await withFetch(async (url, options) => {
  assert.equal(options.headers.authorization, 'public-clickstack-key');
  assert.equal(options.headers['Content-Type'], 'application/json');
  assert.equal(options.credentials, 'omit');
  assert.equal(options.redirect, 'error');
  sent.push({ url, body: options.body });
  if (url.endsWith('/v1/metrics') && metricAttempts++ === 0) return new Response('', { status: 503 });
  return ok();
}, () => sdk.flush(3000));
assert.deepEqual(sent.map(request => request.url), [
  'https://example.invalid/otlp/v1/logs', 'https://example.invalid/otlp/v1/metrics',
  'https://example.invalid/otlp/v1/metrics', 'https://example.invalid/otlp/v1/traces',
]);
assert.equal(sent[1].body, sent[2].body);
assert.equal(sent[0].body.includes('synthetic-token'), false);
assert.equal(sent[0].body.includes('/private/document'), false);
const logs = JSON.parse(sent[0].body).resourceLogs[0];
assert.equal(logs.resource.attributes.find(item => item.key === 'service.name').value.stringValue, 'test.browser');
const error = logs.scopeLogs[0].logRecords.find(record => record.severityNumber === 17);
assert.equal(error.attributes.find(item => item.key === 'telemetry.property.message').value.stringValue, '[redacted]');
const metrics = JSON.parse(sent[1].body).resourceMetrics[0].scopeMetrics[0].metrics;
assert.equal(metrics[0].name, 'telemetry.documents.imported');
assert.equal(metrics[0].sum.aggregationTemporality, 1);
assert.equal(sent[1].body.includes('telemetry.event_id'), false);
const span = JSON.parse(sent[3].body).resourceSpans[0].scopeSpans[0].spans[0];
assert.match(span.traceId, /^[0-9a-f]{32}$/i);
assert.match(span.spanId, /^[0-9a-f]{16}$/i);
assert.ok(BigInt(span.endTimeUnixNano) >= BigInt(span.startTimeUnixNano));
assert.equal(sdk.diagnostics().queue_depth, 0);
await sdk.shutdown();

// A partial response consumes that signal without replay; other signals finish.
const partial = await create({ consent: { usage_aggregate: true, performance: true } });
partial.action('search.submit');
partial.action('search.cancel');
partial.increment('documents.imported', 2);
partial.span('document.ocr')();
const partialPaths = [];
await withFetch(async url => {
  partialPaths.push(url);
  return url.endsWith('/v1/logs')
    ? new Response(JSON.stringify({ partialSuccess: { rejectedLogRecords: '1', errorMessage: 'private-server-message' } }), { status: 200 })
    : ok();
}, () => assert.rejects(partial.flush(), error => !error.message.includes('private-server-message') && /collector rejected/.test(error.message)));
assert.equal(partialPaths.length, 3);
assert.equal(partial.diagnostics().queue_depth, 0);
assert.equal(partial.diagnostics().dropped, 1);
await withFetch(async () => { assert.fail('consumed partial-success records must not be resent'); }, () => partial.flush());
await partial.shutdown();

for (const status of [202, 400, 401, 403, 404, 408, 500]) {
  const rejected = await create({ consent: { usage_aggregate: true } });
  rejected.action('search.submit');
  let calls = 0;
  await withFetch(async () => { calls++; return new Response('private-response', { status }); }, () => assert.rejects(rejected.flush()));
  assert.equal(calls, 1);
  assert.equal(rejected.diagnostics().queue_depth, 0);
  assert.equal(rejected.diagnostics().dropped, 1);
  await rejected.shutdown();
}
for (const body of ['not-json', '[]', 'null', '{"partialSuccess":"wrong"}', '{"partialSuccess":{"rejectedLogRecords":"2"}}', 'x'.repeat(65537), new Uint8Array([255])]) {
  const malformed = await create({ consent: { usage_aggregate: true } });
  malformed.action('search.submit');
  await withFetch(async () => new Response(body, { status: 200 }), () => assert.rejects(malformed.flush()));
  assert.equal(malformed.diagnostics().queue_depth, 0);
  assert.equal(malformed.diagnostics().dropped, 1);
  await malformed.shutdown();
}
const warning = await create({ consent: { usage_aggregate: true } });
warning.action('search.submit');
await withFetch(async () => new Response('{"partialSuccess":{"rejectedLogRecords":"0","errorMessage":"private-warning"}}'), () => warning.flush());
assert.equal(warning.diagnostics().dropped, 0);
await warning.shutdown();

// ProtoJSON int64 accepts equivalent quoted/unquoted decimal and exponent
// forms. Preserve the mathematical value, including numbers JS would round.
for (const count of ['"1e0"', '"1E+0"', '"1.0"', '"10e-1"', '"0.001e3"', '"001e0"', '1.0', '1e0', '10e-1']) {
  const proto = await create({ consent: { usage_aggregate: true } });
  proto.action('search.submit');
  proto.action('search.cancel');
  await withFetch(async () => new Response(`{"partialSuccess":{"rejectedLogRecords":${count}}}`), () => assert.rejects(proto.flush(), /collector rejected/));
  assert.equal(proto.diagnostics().queue_depth, 0);
  assert.equal(proto.diagnostics().dropped, 1);
  await proto.shutdown();
}
for (const count of ['null', '"0e999999999999999"', '"0e-9999"', '"-0.00e10"', '0.0', '-0e0']) {
  const proto = await create({ consent: { usage_aggregate: true } });
  proto.action('search.submit');
  await withFetch(async () => new Response(`{"partialSuccess":{"rejectedLogRecords":${count}}}`), () => proto.flush());
  assert.equal(proto.diagnostics().dropped, 0);
  await proto.shutdown();
}
const unsetPartial = await create({ consent: { usage_aggregate: true } });
unsetPartial.action('search.submit');
await withFetch(async () => new Response('{"partialSuccess":null}'), () => unsetPartial.flush());
assert.equal(unsetPartial.diagnostics().dropped, 0);
await unsetPartial.shutdown();
for (const count of ['"1e-9999"', '1e-9999', '"0.99999999999999999999"', '0.99999999999999999999', '"1.00000000000000000001"', '1.00000000000000000001', '"1.5"', '"-1e0"', '-1e-9999', '"1e999999"', '"+1"', '" 1 "', '"NaN"', '"Infinity"', '"1."']) {
  const proto = await create({ consent: { usage_aggregate: true } });
  proto.action('search.submit');
  proto.action('search.cancel');
  await withFetch(async () => new Response(`{"partialSuccess":{"rejectedLogRecords":${count}}}`), () => assert.rejects(proto.flush(), /invalid response/));
  assert.equal(proto.diagnostics().queue_depth, 0);
  assert.equal(proto.diagnostics().dropped, 2);
  await proto.shutdown();
}

// Consent can change while fetch ignores its AbortSignal. No later prepared
// signal may start after that change; a new flush rebuilds from the Rust queue.
const withdrawn = await create({ consent: { usage_aggregate: true, performance: true } });
withdrawn.action('search.submit');
withdrawn.increment('documents.imported', 2);
withdrawn.span('document.ocr')();
let withdrawalPosts = 0;
await withFetch(async () => {
  withdrawalPosts++;
  withdrawn.setConsent('performance', false);
  return ok();
}, () => assert.rejects(withdrawn.flush(), /cancelled|deadline/));
assert.equal(withdrawalPosts, 1);
assert.equal(withdrawn.diagnostics().queue_depth, 2);
await withFetch(async url => { assert.equal(url.endsWith('/v1/traces'), false); return ok(); }, () => withdrawn.flush());
await withdrawn.shutdown();

const backoffConsent = await create({ consent: { usage_aggregate: true, performance: true } });
backoffConsent.increment('documents.imported', 2);
backoffConsent.span('document.ocr')();
let backoffPosts = 0;
await withFetch(async () => {
  backoffPosts++;
  setTimeout(() => backoffConsent.setConsent('performance', false), 1);
  return new Response('', { status: 503 });
}, () => assert.rejects(backoffConsent.flush(2000), /cancelled|deadline/));
assert.equal(backoffPosts, 1);
assert.equal(backoffConsent.diagnostics().queue_depth, 1);
await withFetch(async url => { assert.equal(url.endsWith('/v1/metrics'), true); return ok(); }, () => backoffConsent.flush());
await backoffConsent.shutdown();

const mutable = {
  app_id: 'test.mutation', app_version: '1.0.0', endpoint: 'https://example.invalid/collector',
  otlp_headers: { authorization: 'public-original' }, consent: { usage_aggregate: false },
};
let resolveInit;
const initializing = createBrowserTelemetry(mutable, { wasmModule: {
  default: () => new Promise(resolve => { resolveInit = resolve; }),
  WasmTelemetry: wasmModule.WasmTelemetry,
} });
mutable.endpoint = 'http://insecure.invalid';
mutable.otlp_headers.authorization = 'changed';
mutable.consent.usage_aggregate = true;
resolveInit();
const snapshotSdk = await initializing;
snapshotSdk.action('search.submit');
assert.equal(snapshotSdk.diagnostics().queue_depth, 0);
snapshotSdk.setConsent('usage_aggregate', true);
snapshotSdk.action('search.submit');
await withFetch(async (url, options) => {
  assert.equal(url, 'https://example.invalid/collector/v1/logs');
  assert.equal(options.headers.authorization, 'public-original');
  return ok();
}, () => snapshotSdk.flush());
snapshotSdk.action('search.submit');
let originalFinished = false;
await withFetch(async () => {
  await new Promise(resolve => setTimeout(resolve, 40));
  return ok();
}, async () => {
  const originalFlush = snapshotSdk.flush(1000).then(() => { originalFinished = true; });
  await assert.rejects(snapshotSdk.flush(1), /deadline exceeded/);
  assert.equal(originalFinished, false);
  await originalFlush;
});
const closing = snapshotSdk.shutdown();
assert.throws(() => snapshotSdk.action('search.submit'), /closed/);
await closing;
await snapshotSdk.shutdown();

// A streaming response cannot outlive the caller deadline or allocate freely.
const streaming = await create({ consent: { usage_aggregate: true } });
streaming.action('search.submit');
await withFetch(async () => new Response(new ReadableStream({ start() {} })), () => assert.rejects(streaming.flush(20), /cancelled|deadline/));
assert.equal(streaming.diagnostics().queue_depth, 1);
await withFetch(async () => ok(), () => streaming.flush());
await streaming.shutdown();

const oversizedSpanEngine = new wasmModule.WasmTelemetry(JSON.stringify({ app_id: 'test.encoding', app_version: '1.0', consent: { performance: true } }));
oversizedSpanEngine.command(JSON.stringify({ op: 'span', name: 'document.ocr', duration_ms: Number.MAX_VALUE }), Date.now());
const encodingFailure = new BrowserTelemetry(oversizedSpanEngine, { endpoint: 'https://example.invalid' });
await withFetch(async () => assert.fail('unrepresentable data must not reach the network'), () => assert.rejects(encodingFailure.flush(), /cannot be encoded/));
assert.equal(encodingFailure.diagnostics().queue_depth, 0);
assert.equal(encodingFailure.diagnostics().dropped, 1);
await encodingFailure.shutdown();
oversizedSpanEngine.free();

for (const config of [
  { export_protocol: 'otlp_grpc' }, { export_protocol: null },
  { endpoint: 'http://example.invalid', allow_insecure_loopback: true },
  { endpoint: 'http://localhost:4318', allow_insecure_loopback: true },
  { endpoint: 'http://127.1:4318', allow_insecure_loopback: true },
  { endpoint: 'http://2130706433:4318', allow_insecure_loopback: true },
  { endpoint: 'https://example.invalid/v1/logs' }, { endpoint: 'https://example.invalid/v1' },
  { endpoint: 'https://example.invalid/a/../b' }, { endpoint: 'https://example.invalid/a//b' },
  { endpoint: 'https://example.invalid/a%2fb' }, { endpoint: 'https://example.invalid?' },
  { ingestion_token: 'public', otlp_headers: { authorization: 'public' } },
  { otlp_headers: { host: 'private.invalid' } }, { otlp_headers: { Cookie: 'secret' } },
  { otlp_headers: { 'proxy-authorization': 'secret' } }, { otlp_headers: { 'sec-fetch-site': 'same-origin' } },
  { otlp_headers: { Authorization: 'one', authorization: 'two' } },
  { otlp_headers: { authorization: ' space ' } }, { otlp_headers: { authorization: 'line\nbreak' } },
  { otlp_headers: Array(2) }, { otlp_headers: null },
  { export_protocol: 'legacy_json', endpoint: 'https://example.invalid/v1/telemetry/batches', otlp_headers: { 'x-api-key': 'key' } },
]) await assert.rejects(create(config));
const loopback = await create({ endpoint: 'http://127.0.0.1:4318', allow_insecure_loopback: true, consent: { usage_aggregate: true } });
loopback.action('search.submit');
await withFetch(async url => { assert.equal(url, 'http://127.0.0.1:4318/v1/logs'); return ok(); }, () => loopback.flush());
await loopback.shutdown();

// Existing custom gateway JSON is explicit and keeps its prior status behavior.
const legacy = await create({ endpoint: 'https://example.invalid/v1/telemetry/batches', export_protocol: 'legacy_json', ingestion_token: 'public-legacy', consent: { usage_aggregate: true } });
legacy.action('search.submit');
await withFetch(async (url, options) => {
  assert.equal(url, 'https://example.invalid/v1/telemetry/batches');
  assert.equal(options.headers.authorization, 'Bearer public-legacy');
  assert.equal(JSON.parse(options.body).schema_version, 1);
  return new Response('', { status: 202 });
}, () => legacy.flush());
await legacy.shutdown();

const failed = await createBrowserTelemetry({ app_id: 'test.offline', app_version: '1.0.0', consent: { usage_aggregate: true } }, { wasmModule });
failed.action('search.submit');
await assert.rejects(failed.shutdown());
assert.throws(() => failed.action('search.submit'));
const raw = new wasmModule.WasmTelemetry(JSON.stringify({ app_id: 'test.strict', app_version: '1.0.0' }));
assert.throws(() => raw.command('{"op":"diagnostics","extra":true}', Date.now()));
assert.throws(() => raw.command('{"op":"acknowledge","ids":[],"rejected":1}', Date.now()));
assert.equal(raw.response_rejected('logs', '{"partialSuccess":{"rejectedLogRecords":"1e0"}}', 2), 1);
assert.equal(raw.response_rejected('metrics', '{"partialSuccess":{"rejectedDataPoints":null}}', 2), 0);
assert.equal(raw.response_rejected('traces', '{"partialSuccess":{"rejectedSpans":"1.5e1"}}', 20), 15);
assert.throws(() => raw.response_rejected('logs', '{"partialSuccess":{"rejectedLogRecords":1e-9999}}', 2));
assert.throws(() => raw.response_rejected('unsupported', '{}', 2));
assert.throws(() => raw.response_rejected('logs', ' '.repeat(65537), 2));
raw.free();
console.log('Browser OTLP/legacy WASM smoke passed (Node host, mocked collector)');
