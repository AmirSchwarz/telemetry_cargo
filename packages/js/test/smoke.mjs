import assert from 'node:assert/strict';
import { createTelemetry } from '../index.mjs';
import { mkdtemp, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

const sdk = await createTelemetry({ app_id: 'test.node', app_version: '1.0.0' });
assert.equal((await sdk.capabilities()).binding, 'napi');
await sdk.action('search.submit');
await sdk.captureError('error.test', 'synthetic-token /private/name');
assert.equal((await sdk.diagnostics()).queue_depth, 0);
await sdk.setConsent('usage_aggregate', true);
await sdk.action('search.submit');
await sdk.increment('documents.imported', 3);
assert.equal((await sdk.diagnostics()).queue_depth, 2);
await assert.rejects(sdk.event('action', { id: 'search.submit', result: 'success', query: 'secret' }));
await assert.rejects(sdk.flush(10));
assert.equal((await sdk.diagnostics()).queue_depth, 2);
await sdk.setConsent('performance', true);
const finish = await sdk.span('document.ocr');
await finish();
await finish();
assert.equal((await sdk.diagnostics()).queue_depth, 3);
const old = await sdk.span('document.ocr');
await sdk.setConsent('performance', false);
await sdk.setConsent('performance', true);
await old();
assert.equal((await sdk.diagnostics()).queue_depth, 2);
await sdk.setConsent('usage_aggregate', false);
await sdk.setConsent('performance', false);
assert.equal((await sdk.diagnostics()).queue_depth, 0);
const closing = sdk.shutdown();
await assert.rejects(sdk.action('search.submit'), /closed/);
await closing;
await sdk.shutdown();
await assert.rejects(sdk.action('search.submit'));

const temp = await mkdtemp(join(tmpdir(), 'telemetry-node-'));
try {
  const disk = await createTelemetry({ app_id: 'test.node.sqlite', app_version: '1.0.0', sqlite_path: join(temp, 'queue.db'), consent: { usage_aggregate: true } });
  const action = disk.action('search.submit');
  assert.ok(action instanceof Promise);
  await action;
  assert.equal((await disk.diagnostics()).queue_depth, 1);
  await disk.setConsent('usage_aggregate', false);
  await disk.shutdown();
} finally { await rm(temp, { recursive: true, force: true }); }
console.log('Node native smoke passed');
