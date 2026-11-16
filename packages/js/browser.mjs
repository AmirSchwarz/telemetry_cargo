/** Browser adapter for the thread-free Rust/WASM engine. Memory queue only. */
export async function createBrowserTelemetry(config, options = {}) {
  const { endpoint, ingestion_token, ...engineConfig } = config;
  validateAdapterConfig(endpoint, ingestion_token);
  const engineConfigJson = JSON.stringify(engineConfig);
  const adapterConfig = { endpoint, ingestion_token, consent: JSON.parse(engineConfigJson).consent };
  const wasm = options.wasmModule ?? await import('./wasm/telemetry_wasm.js');
  if (typeof wasm.default === 'function') await wasm.default(options.wasmInput);
  return new BrowserTelemetry(new wasm.WasmTelemetry(engineConfigJson), adapterConfig);
}

export class BrowserTelemetry {
  #engine;
  #endpoint;
  #token;
  #request;
  #flush;
  #performance;
  #generation = 0;
  #closing;

  constructor(engine, config) {
    const { endpoint, ingestion_token, consent } = config;
    validateAdapterConfig(endpoint, ingestion_token);
    this.#engine = engine;
    this.#endpoint = endpoint;
    this.#token = ingestion_token;
    this.#performance = consent?.performance === true;
  }

  #command(op, fields = {}) {
    if (this.#closing && !['batch', 'acknowledge', 'shutdown', 'diagnostics', 'capabilities', 'set_consent'].includes(op)) {
      throw new Error('telemetry is closed');
    }
    return JSON.parse(this.#engine.command(JSON.stringify({ op, ...fields }), Date.now()));
  }

  setConsent(category, enabled) {
    // Abort a request that started before withdrawal; sent data cannot be retracted.
    if (!enabled) this.#request?.abort();
    if (category === 'performance') {
      this.#performance = false;
      this.#generation++;
    }
    this.#command('set_consent', { category, enabled });
    if (category === 'performance') this.#performance = enabled;
  }
  event(name, properties = {}) { this.#command('event', { name, properties }); }
  registerEvent(definition) { this.#command('register_event', { definition }); }
  action(id, { result = 'success' } = {}) { this.#command('action', { id, result }); }
  captureError(code, message = '') { this.#command('capture_error', { code, message }); }
  increment(name, amount = 1) { this.#command('increment', { name, amount }); }
  diagnostics() { return this.#command('diagnostics'); }
  capabilities() { return this.#command('capabilities'); }
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

  /** Retries are bounded by timeoutMs and queue TTL. Only successful batches ack. */
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
      let batch = this.#command('batch', { limit: 128 });
      if (batch.events.length === 0) return;
      if (!this.#endpoint) throw new Error('telemetry endpoint is not configured');
      const remaining = deadline - performance.now();
      if (remaining <= 0) throw new Error('telemetry flush deadline exceeded');
      const controller = new AbortController();
      this.#request = controller;
      const timer = setTimeout(() => controller.abort(), remaining);
      let response;
      let waitMs;
      try {
        response = await fetch(this.#endpoint, {
          method: 'POST',
          headers: {
            'Content-Type': 'application/json',
            ...(this.#token ? { Authorization: `Bearer ${this.#token}` } : {}),
          },
          body: JSON.stringify(batch),
          signal: controller.signal,
          credentials: 'omit',
          redirect: 'error',
          referrerPolicy: 'no-referrer',
        });
        // Status is the only response data used; stop arbitrary streaming bodies.
        void response.body?.cancel().catch(() => {});
        if (response.ok) {
          this.#command('acknowledge', { ids: batch.events.map(event => event.event_id) });
          attempt = 0;
          continue;
        }
        if (!(response.status === 408 || response.status === 429 || response.status >= 500)) {
          throw new PermanentUploadError();
        }
        const retry = response.headers.get('Retry-After');
        if (retry != null) {
          waitMs = /^\d+$/.test(retry) ? Number(retry) * 1000 : Math.max(0, Date.parse(retry) - Date.now());
        }
      } catch (error) {
        if (error instanceof PermanentUploadError) throw new Error('telemetry gateway rejected batch');
        if (controller.signal.aborted) throw new Error('telemetry upload cancelled or deadline exceeded');
        // Network failures retain events for bounded retries and later flushes.
      } finally {
        clearTimeout(timer);
        if (this.#request === controller) this.#request = undefined;
      }
      // Failed batches remain only in the consent-aware Rust queue during backoff.
      batch = undefined;
      const backoff = Math.min(30_000, 250 * (2 ** Math.min(attempt++, 7))) * (0.75 + Math.random() * 0.5);
      const delay = Number.isFinite(waitMs) ? Math.max(backoff, waitMs) : backoff;
      if (performance.now() + delay >= deadline) throw new Error('telemetry flush deadline exceeded');
      await new Promise(resolve => setTimeout(resolve, delay));
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
      this.#performance = false;
      this.#generation++;
      this.#command('shutdown');
    }
  }
  async close(timeoutMs = 5000) { await this.shutdown(timeoutMs); }
}

class PermanentUploadError extends Error {}

function withinDeadline(pending, timeoutMs) {
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => reject(new Error('telemetry flush deadline exceeded')), timeoutMs);
    pending.then(
      value => { clearTimeout(timer); resolve(value); },
      error => { clearTimeout(timer); reject(error); },
    );
  });
}

function validateAdapterConfig(endpoint, token) {
  if (endpoint !== undefined && endpoint !== null) {
    if (typeof endpoint !== 'string' || endpoint.length > 4096) throw new Error('invalid telemetry endpoint');
    const url = new URL(endpoint);
    if (url.protocol !== 'https:' || url.username || url.password || url.hash || url.search || url.pathname !== '/v1/telemetry/batches') {
      throw new Error('telemetry endpoint must be HTTPS at /v1/telemetry/batches without credentials, query or fragment');
    }
  }
  if (token != null && (typeof token !== 'string' || token.length > 1024 || /[^\x21-\x7e]/.test(token))) {
    throw new Error('invalid telemetry ingestion token');
  }
}
