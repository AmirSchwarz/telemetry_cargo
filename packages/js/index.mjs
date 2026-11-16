import { createRequire } from 'node:module';
import { performance } from 'node:perf_hooks';

const require = createRequire(import.meta.url);

/** Create a native SDK. All optional telemetry is disabled until consent. */
export async function createTelemetry(config) {
  const { createNativeTelemetry } = require('./native/telemetry.node');
  const snapshot = JSON.parse(JSON.stringify(config));
  return new Telemetry(await createNativeTelemetry(JSON.stringify(snapshot)), snapshot);
}

export class Telemetry {
  #native;
  #performance;
  #generation = 0;
  #closing;

  constructor(native, config) {
    this.#native = native;
    this.#performance = config.consent?.performance === true;
  }

  async #async(op, fields = {}) {
    if (this.#closing) throw new Error('telemetry is closed');
    return JSON.parse(await this.#native.commandAsync(JSON.stringify({ op, ...fields })));
  }

  async setConsent(category, enabled) {
    if (category === 'performance') {
      // Clear immediately when withdrawing, before waiting for a request to finish.
      this.#performance = false;
      this.#generation++;
    }
    await this.#async('set_consent', { category, enabled });
    if (category === 'performance') this.#performance = enabled;
  }

  async event(name, properties = {}) { await this.#async('event', { name, properties }); }
  async registerEvent(definition) { await this.#async('register_event', { definition }); }
  async action(id, { result = 'success' } = {}) { await this.#async('action', { id, result }); }
  async captureError(code, message = '') { await this.#async('capture_error', { code, message }); }
  async increment(name, amount = 1) { await this.#async('increment', { name, amount }); }
  async screenEnter(id) { await this.#async('screen_enter', { id }); }
  async screenLeave(id) { await this.#async('screen_leave', { id }); }
  async workflowStart(name) { return (await this.#async('workflow_start', { name })).id; }
  async workflowStep(id, step) { await this.#async('workflow_step', { id, step }); }
  async workflowEnd(id, outcome = 'complete') { await this.#async('workflow_end', { id, outcome }); }
  async lifecycle(windowId, visible, focused) { await this.#async('lifecycle', { window_id: windowId, visible, focused }); }
  async interaction() { await this.#async('interaction'); }
  async suspend(suspended) { await this.#async('suspend', { suspended }); }
  async checkpoint() { await this.#async('checkpoint'); }
  async diagnostics() { return this.#async('diagnostics'); }
  async capabilities() { return { ...await this.#async('capabilities'), binding: 'napi' }; }

  /** Return an idempotent finish function. No start is retained while disabled. */
  async span(name) {
    if (!this.#performance) return async () => {};
    const diagnostics = await this.diagnostics();
    if (!diagnostics.consent.performance) return async () => {};
    const start = performance.now();
    const generation = this.#generation;
    const epoch = diagnostics.consent_epoch;
    let finished = false;
    return async () => {
      if (finished) return;
      finished = true;
      if (this.#performance && this.#generation === generation) {
        await this.#async('span', { name, duration_ms: performance.now() - start, consent_epoch: epoch });
      }
    };
  }

  async flush(timeoutMs = 5000) { await this.#async('flush', { timeout_ms: timeoutMs }); }
  async shutdown(timeoutMs = 5000) {
    if (this.#closing) return this.#closing;
    this.#performance = false;
    this.#generation++;
    this.#closing = Promise.resolve().then(() => this.#native.close(timeoutMs));
    return this.#closing;
  }
  async close(timeoutMs = 5000) { await this.shutdown(timeoutMs); }
}
