import type { ActionResult, Capabilities, Category, Diagnostics, EventDefinition, Limits, QueueConfig } from './index.js';
export interface BrowserConfig {
  app_id: string; app_version: string;
  endpoint?: string | null; ingestion_token?: string | null;
  consent?: Partial<Record<Category, boolean>>;
  essential_operations_reviewed?: boolean;
  queue?: QueueConfig; limits?: Limits;
}
export declare function createBrowserTelemetry(config: BrowserConfig, options?: { wasmModule?: unknown; wasmInput?: unknown }): Promise<BrowserTelemetry>;
export declare class BrowserTelemetry {
  private constructor();
  setConsent(category: Category, enabled: boolean): void;
  event(name: string, properties?: Record<string, unknown>): void;
  registerEvent(definition: EventDefinition): void;
  action(id: string, options?: { result?: ActionResult }): void;
  captureError(code: string, message?: string): void;
  increment(name: string, amount?: number): void;
  span(name: string): () => void;
  diagnostics(): Diagnostics;
  capabilities(): Capabilities;
  flush(timeoutMs?: number): Promise<void>;
  shutdown(timeoutMs?: number): Promise<void>;
  close(timeoutMs?: number): Promise<void>;
}
