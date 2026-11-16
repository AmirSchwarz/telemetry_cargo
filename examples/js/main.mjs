import { createTelemetry } from '../../packages/js/index.mjs';

const sdk = await createTelemetry({ app_id: 'example.node', app_version: '1.0.0' });
await sdk.action('search.submit'); // Disabled until consent.
await sdk.setConsent('usage_aggregate', true);
await sdk.action('search.submit', { result: 'success' });
await sdk.increment('documents.imported', 3);
await sdk.setConsent('diagnostics', true);
await sdk.captureError('document.failed', 'synthetic secret is redacted');
console.log(await sdk.capabilities(), await sdk.diagnostics());
// No endpoint in this offline example. Withdrawal purges the demonstration data.
await sdk.setConsent('usage_aggregate', false);
await sdk.setConsent('diagnostics', false);
await sdk.shutdown();
