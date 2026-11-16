/** Browser adapter for the thread-free Rust/WASM engine. Memory queue only. */
export async function createBrowserTelemetry(config, options = {}) {
  // Capture every mutable field before loading WASM or awaiting initialization.
  const snapshot = snapshotConfig(config);
  const { endpoint, ingestion_token, export_protocol, otlp_headers, allow_insecure_loopback, ...engineConfig } = snapshot;
  const adapter = validateAdapterConfig({ endpoint, ingestion_token, export_protocol, otlp_headers, allow_insecure_loopback });
  const engineConfigJson = JSON.stringify(engineConfig);
  const wasm = options.wasmModule ?? await import('./wasm/telemetry_wasm.js');
  if (typeof wasm.default === 'function') await wasm.default(options.wasmInput);
  return new BrowserTelemetry(new wasm.WasmTelemetry(engineConfigJson), { ...adapter, consent: engineConfig.consent });
}

const MAX_BODY_BYTES = 4 * 1024 * 1024;
const MAX_RESPONSE_BYTES = 64 * 1024;
const RETRYABLE_OTLP = new Set([429, 502, 503, 504]);
const ENCODER = new TextEncoder();

export class BrowserTelemetry {
  #engine;
  #endpoint;
  #headers;
  #protocol;
  #request;
  #flush;
  #performance;
  #generation = 0;
  #consentGeneration = 0;
  #closing;

  constructor(engine, config) {
    const snapshot = snapshotConfig(config);
    const adapter = validateAdapterConfig(snapshot);
    this.#engine = engine;
    this.#endpoint = adapter.endpoint;
    this.#headers = adapter.headers;
    this.#protocol = adapter.export_protocol;
    this.#performance = snapshot.consent?.performance === true;
  }

  #command(op, fields = {}) {
    if (this.#closing && !['batch', 'otlp_batch', 'acknowledge', 'shutdown', 'diagnostics', 'capabilities', 'set_consent'].includes(op)) {
      throw new Error('telemetry is closed');
    }
    return JSON.parse(this.#engine.command(JSON.stringify({ op, ...fields }), Date.now()));
  }

  #checkGeneration(generation, signal) {
    if (generation !== this.#consentGeneration || signal?.aborted) {
      throw new CancelledUploadError();
    }
  }

  setConsent(category, enabled) {
    this.#command('set_consent', { category, enabled });
    // Invalidate the whole prepared multi-signal snapshot, including allowed
    // signals that have not started yet. The next flush reads the Rust queue.
    this.#consentGeneration++;
    this.#request?.abort();
    if (category === 'performance') {
      this.#performance = enabled;
      this.#generation++;
    }
  }
  event(name, properties = {}) { this.#command('event', { name, properties }); }
  registerEvent(definition) { this.#command('register_event', { definition }); }
  action(id, { result = 'success' } = {}) { this.#command('action', { id, result }); }
  captureError(code, message = '') { this.#command('capture_error', { code, message }); }
  increment(name, amount = 1) { this.#command('increment', { name, amount }); }
  diagnostics() { return this.#command('diagnostics'); }
  capabilities() { return { ...this.#command('capabilities'), export_protocol: this.#protocol }; }
  span(name) {
    if (!this.#performance) return () => {};
    const start = performance.now();
    const generation = this.#generation;
    let finished = false;
    return () => {
      if (finished) return;
      finished = true;
      if (this.#performance && generation === this.#generation) {
        this.#command('span', { name, duration_ms: performance.now() - start });
      }
    };
  }

  /** Each signal acknowledges independently; accepted signals are never replayed. */
  async flush(timeoutMs = 5000) {
    if (!Number.isFinite(timeoutMs) || timeoutMs < 0 || timeoutMs > 300_000) {
      throw new Error('timeoutMs must be between 0 and 300000');
    }
    if (this.#flush) return withinDeadline(this.#flush, timeoutMs);
    const pending = this.#send(timeoutMs);
    this.#flush = pending;
    try { await pending; } finally { this.#flush = undefined; }
  }

  async #send(timeoutMs) {
    const deadline = performance.now() + timeoutMs;
    let attempt = 0;
    while (true) {
      const generation = this.#consentGeneration;
      let requests;
      if (this.#protocol === 'legacy_json') {
        const batch = this.#command('batch', { limit: 128 });
        requests = batch.events.length ? [{ signal: 'legacy', path: '', event_ids: batch.events.map(event => event.event_id), body: batch }] : [];
      } else {
        try {
          requests = this.#command('otlp_batch', { limit: 128 });
        } catch {
          // Match native permanent encoding failures: remove this page rather
          // than stranding the queue behind a value OTLP cannot represent.
          const batch = this.#command('batch', { limit: 128 });
          const ids = batch.events.map(event => event.event_id);
          this.#command('acknowledge', { ids, rejected: ids.length });
          throw new Error('telemetry batch cannot be encoded as OTLP');
        }
      }
      if (requests.length === 0) return;
      if (!this.#endpoint) throw new Error('telemetry endpoint is not configured');
      let encodedBytes = 0;
      for (const request of requests) {
        request.encoded = JSON.stringify(request.body);
        encodedBytes += ENCODER.encode(request.encoded).byteLength;
      }
      if (encodedBytes > MAX_BODY_BYTES) {
        const ids = requests.flatMap(request => request.event_ids);
        this.#command('acknowledge', { ids, rejected: ids.length });
        throw new Error('telemetry batch exceeds encoding budget');
      }
      let transient = false;
      let waitMs;
      let permanent = false;
      for (const request of requests) {
        this.#checkGeneration(generation);
        const remaining = deadline - performance.now();
        if (remaining <= 0) throw new Error('telemetry flush deadline exceeded');
        const controller = new AbortController();
        this.#request = controller;
        const timer = setTimeout(() => controller.abort(), remaining);
        let response;
        try {
          response = await abortable(fetch(`${this.#endpoint}${request.path}`, {
            method: 'POST',
            headers: { 'Content-Type': 'application/json', ...this.#headers },
            body: request.encoded,
            signal: controller.signal,
            credentials: 'omit',
            redirect: 'error',
            referrerPolicy: 'no-referrer',
          }), controller.signal);
          this.#checkGeneration(generation, controller.signal);
          if (this.#protocol === 'legacy_json') {
            cancelBody(response);
            if (response.ok) {
              this.#command('acknowledge', { ids: request.event_ids });
              attempt = 0;
              continue;
            }
            if (!(response.status === 408 || response.status === 429 || response.status >= 500)) {
              throw new PermanentUploadError();
            }
          } else if (response.status === 200) {
            let rejected;
            try {
              const body = await boundedText(response, controller.signal, () => this.#checkGeneration(generation, controller.signal));
              this.#checkGeneration(generation, controller.signal);
              try { rejected = this.#engine.response_rejected(request.signal, body, request.event_ids.length); }
              catch { throw new InvalidResponseError(); }
            } catch (error) {
              if (!(error instanceof InvalidResponseError)) throw error;
              // A malformed success may already have accepted data. Never replay it.
              rejected = request.event_ids.length;
              permanent = true;
            }
            this.#command('acknowledge', { ids: request.event_ids, rejected });
            permanent ||= rejected > 0;
            attempt = 0;
            continue;
          } else {
            cancelBody(response);
            if (!RETRYABLE_OTLP.has(response.status)) {
              this.#command('acknowledge', { ids: request.event_ids, rejected: request.event_ids.length });
              permanent = true;
              continue;
            }
          }
          const retry = response.headers.get('Retry-After');
          if (retry != null) {
            waitMs = /^\d+$/.test(retry) ? Number(retry) * 1000 : Math.max(0, Date.parse(retry) - Date.now());
          }
          transient = true;
          break;
        } catch (error) {
          cancelBody(response);
          if (error instanceof PermanentUploadError) throw new Error('telemetry gateway rejected batch');
          if (error instanceof CancelledUploadError || controller.signal.aborted || generation !== this.#consentGeneration) {
            throw new Error('telemetry upload cancelled or deadline exceeded');
          }
          // Network failures retain events for bounded retries and later flushes.
          transient = true;
          break;
        } finally {
          clearTimeout(timer);
          if (this.#request === controller) this.#request = undefined;
        }
      }
      if (permanent) throw new Error('telemetry collector rejected records or returned an invalid response');
      if (!transient) continue;
      // Release all prepared payloads before retry/backoff. Consent remains in Rust.
      requests = undefined;
      const backoff = Math.min(30_000, 250 * (2 ** Math.min(attempt++, 7))) * (0.75 + Math.random() * 0.5);
      const delay = Number.isFinite(waitMs) ? Math.max(backoff, waitMs) : backoff;
      if (performance.now() + delay >= deadline) throw new Error('telemetry flush deadline exceeded');
      await new Promise(resolve => setTimeout(resolve, delay));
      this.#checkGeneration(generation);
    }
  }

  async shutdown(timeoutMs = 5000) {
    if (this.#closing) return this.#closing;
    this.#performance = false;
    this.#generation++;
    this.#closing = this.#shutdown(timeoutMs);
    return this.#closing;
  }

  async #shutdown(timeoutMs) {
    try {
      await this.flush(timeoutMs);
    } finally {
      this.#request?.abort();
      this.#consentGeneration++;
      this.#performance = false;
      this.#generation++;
      this.#command('shutdown');
    }
  }
  async close(timeoutMs = 5000) { await this.shutdown(timeoutMs); }
}

class PermanentUploadError extends Error {}
class InvalidResponseError extends Error {}
class CancelledUploadError extends Error {
  constructor() { super('telemetry upload cancelled or deadline exceeded'); }
}

function cancelBody(response) {
  if (response?.body && !response.body.locked) void response.body.cancel().catch(() => {});
}

function abortable(pending, signal) {
  if (signal.aborted) return Promise.reject(new CancelledUploadError());
  return new Promise((resolve, reject) => {
    const abort = () => reject(new CancelledUploadError());
    signal.addEventListener('abort', abort, { once: true });
    Promise.resolve(pending).then(
      value => { signal.removeEventListener('abort', abort); resolve(value); },
      error => { signal.removeEventListener('abort', abort); reject(error); },
    );
  });
}

async function boundedText(response, signal, check) {
  if (!response.body) throw new InvalidResponseError();
  const reader = response.body.getReader();
  const chunks = [];
  let size = 0;
  try {
    while (true) {
      const { done, value } = await abortable(reader.read(), signal);
      check();
      if (done) break;
      size += value.byteLength;
      if (size > MAX_RESPONSE_BYTES) throw new InvalidResponseError();
      chunks.push(value);
    }
    const bytes = new Uint8Array(size);
    let offset = 0;
    for (const chunk of chunks) { bytes.set(chunk, offset); offset += chunk.byteLength; }
    try {
      return new TextDecoder('utf-8', { fatal: true }).decode(bytes);
    }
    catch { throw new InvalidResponseError(); }
  } finally {
    void reader.cancel().catch(() => {});
    reader.releaseLock();
  }
}

function withinDeadline(pending, timeoutMs) {
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => reject(new Error('telemetry flush deadline exceeded')), timeoutMs);
    pending.then(
      value => { clearTimeout(timer); resolve(value); },
      error => { clearTimeout(timer); reject(error); },
    );
  });
}

function snapshotConfig(config) {
  if (!config || typeof config !== 'object' || Array.isArray(config)) throw new Error('invalid telemetry configuration');
  try { return JSON.parse(JSON.stringify(config)); }
  catch { throw new Error('invalid telemetry configuration'); }
}

function validateAdapterConfig(config) {
  const protocol = config.export_protocol ?? 'otlp_http_json';
  if (!['otlp_http_json', 'legacy_json'].includes(protocol) || config.export_protocol === null) {
    throw new Error('invalid telemetry export protocol');
  }
  const allowLoopback = config.allow_insecure_loopback ?? false;
  if (typeof allowLoopback !== 'boolean' || config.allow_insecure_loopback === null) throw new Error('invalid telemetry loopback option');
  if (config.otlp_headers === null) throw new Error('invalid telemetry headers');
  const customHeaders = config.otlp_headers ?? {};
  if (customHeaders === null || typeof customHeaders !== 'object' || Array.isArray(customHeaders)) throw new Error('invalid telemetry headers');
  const headers = Object.create(null);
  const forbidden = new Set(['host', 'content-length', 'content-type', 'content-encoding', 'accept', 'accept-encoding', 'connection', 'keep-alive', 'te', 'trailer', 'transfer-encoding', 'upgrade', 'cookie', 'cookie2', 'set-cookie', 'set-cookie2', 'user-agent', 'origin', 'referer']);
  let headerBytes = 0;
  for (const [name, value] of Object.entries(customHeaders)) {
    const normalized = name.toLowerCase();
    if (!name || name.length > 64 || !/^[!#$%&'*+\-.^_`|~0-9A-Za-z]+$/.test(name)
      || Object.hasOwn(headers, normalized) || forbidden.has(normalized) || normalized.startsWith('proxy-') || normalized.startsWith('sec-')
      || typeof value !== 'string' || !value || value.length > 2048 || value.trim() !== value || /[^\x20-\x7e]/.test(value)) {
      throw new Error('invalid telemetry headers');
    }
    headers[normalized] = value;
    headerBytes += name.length + value.length;
  }
  const token = config.ingestion_token;
  if (token != null) {
    if (typeof token !== 'string' || !token || token.length > 2048 || /[^\x21-\x7e]/.test(token) || Object.hasOwn(headers, 'authorization')) {
      throw new Error('invalid telemetry ingestion token');
    }
    headers.authorization = `Bearer ${token}`;
    headerBytes += 'authorization'.length + headers.authorization.length;
  }
  if (Object.keys(headers).length > 16 || headerBytes > 8192) throw new Error('invalid telemetry headers');
  if (protocol === 'legacy_json' && (Object.keys(customHeaders).length > 0 || allowLoopback)) throw new Error('OTLP options cannot be used with legacy export');

  let endpoint = config.endpoint;
  if (endpoint != null) {
    if (typeof endpoint !== 'string' || !endpoint || ENCODER.encode(endpoint).byteLength > 2048 || /[\x00-\x20\x7f%\\]/.test(endpoint) || endpoint.includes('?') || endpoint.includes('#')) {
      throw new Error('invalid telemetry endpoint');
    }
    let url;
    try { url = new URL(endpoint); } catch { throw new Error('invalid telemetry endpoint'); }
    const rawAuthority = endpoint.split('://')[1]?.split('/')[0] ?? '';
    if (rawAuthority.includes('@') || url.username || url.password) throw new Error('invalid telemetry endpoint');
    if (protocol === 'legacy_json') {
      if (url.protocol !== 'https:' || url.pathname !== '/v1/telemetry/batches') throw new Error('invalid legacy telemetry endpoint');
      endpoint = url.href;
    } else {
      const loopback = /^(?:127\.0\.0\.1|\[::1\])(?::[0-9]+)?$/.test(rawAuthority);
      if (!(url.protocol === 'https:' || (allowLoopback && loopback && url.protocol === 'http:'))) throw new Error('telemetry endpoint must use HTTPS');
      const rawPath = endpoint.split('://')[1]?.slice(rawAuthority.length) ?? '';
      const segments = rawPath.replace(/\/+$/, '').split('/').slice(1);
      if (segments.some(segment => !segment || segment === '.' || segment === '..') || /\/v1\/(logs|metrics|traces|telemetry\/batches)\/?$/.test(url.pathname) || /\/v1\/?$/.test(url.pathname)) {
        throw new Error('telemetry endpoint must be a collector base URL');
      }
      endpoint = url.href.replace(/\/+$/, '');
    }
  }
  return { endpoint, ingestion_token: token, export_protocol: protocol, otlp_headers: { ...customHeaders }, allow_insecure_loopback: allowLoopback, headers };
}
