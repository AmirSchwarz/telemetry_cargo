export type Category = 'essential_operations' | 'diagnostics' | 'performance' | 'usage_aggregate' | 'usage_session' | 'extended_support';
export type ActionResult = 'success' | 'failure' | 'cancelled';
export type WorkflowOutcome = 'complete' | 'cancelled' | 'failed' | 'timeout';
export type ExportProtocol = 'otlp_http_json' | 'legacy_json';
export interface QueueConfig { max_events?: number; max_bytes?: number; ttl_ms?: number; per_category_max_events?: number; }
export interface Limits { max_properties?: number; max_string_bytes?: number; max_event_bytes?: number; }
export interface Config {
  app_id: string; app_version: string;
  endpoint?: string | null; ingestion_token?: string | null;
  export_protocol?: ExportProtocol;
  otlp_headers?: Record<string, string>;
  allow_insecure_loopback?: boolean;
  consent?: Partial<Record<Category, boolean>>;
  essential_operations_reviewed?: boolean;
  queue?: QueueConfig; limits?: Limits;
  idle_threshold_ms?: number; sqlite_path?: string | null;
  batch_size?: number; flush_interval_ms?: number;
}
export interface Diagnostics { queue_depth: number; queued_bytes: number; [key: string]: unknown; }
export interface Capabilities { binding: string; [key: string]: unknown; }
export interface EventDefinition { name: string; category: Category; properties: Record<string, unknown>; }
export declare function createTelemetry(config: Config): Promise<Telemetry>;
export declare class Telemetry {
  private constructor();
  setConsent(category: Category, enabled: boolean): Promise<void>;
  event(name: string, properties?: Record<string, unknown>): Promise<void>;
  registerEvent(definition: EventDefinition): Promise<void>;
  action(id: string, options?: { result?: ActionResult }): Promise<void>;
  captureError(code: string, message?: string): Promise<void>;
  increment(name: string, amount?: number): Promise<void>;
  screenEnter(id: string): Promise<void>;
  screenLeave(id: string): Promise<void>;
  workflowStart(name: string): Promise<string>;
  workflowStep(id: string, step: string): Promise<void>;
  workflowEnd(id: string, outcome?: WorkflowOutcome): Promise<void>;
  lifecycle(windowId: string, visible: boolean, focused: boolean): Promise<void>;
  interaction(): Promise<void>;
  suspend(suspended: boolean): Promise<void>;
  checkpoint(): Promise<void>;
  span(name: string): Promise<() => Promise<void>>;
  diagnostics(): Promise<Diagnostics>;
  capabilities(): Promise<Capabilities>;
  flush(timeoutMs?: number): Promise<void>;
  shutdown(timeoutMs?: number): Promise<void>;
  close(timeoutMs?: number): Promise<void>;
}
