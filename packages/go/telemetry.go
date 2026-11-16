// Package telemetry wraps the Rust engine through its stable versioned C ABI.
// It collects no optional telemetry until the matching consent is enabled.
package telemetry

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"sync"
	"time"
)

type Category string

const (
	EssentialOperations Category = "essential_operations"
	Diagnostics         Category = "diagnostics"
	Performance         Category = "performance"
	UsageAggregate      Category = "usage_aggregate"
	UsageSession        Category = "usage_session"
	ExtendedSupport     Category = "extended_support"
)

type ActionResult string

const (
	Success   ActionResult = "success"
	Failure   ActionResult = "failure"
	Cancelled ActionResult = "cancelled"
)

type WorkflowOutcome string

const (
	Complete          WorkflowOutcome = "complete"
	WorkflowCancelled WorkflowOutcome = "cancelled"
	Failed            WorkflowOutcome = "failed"
	Timeout           WorkflowOutcome = "timeout"
)

// Config uses the same JSON contract as Rust. Omitted consent categories are off.
// Native queue persistence is opt-in and is not encrypted by SQLite itself.
type Config struct {
	AppID                       string            `json:"app_id"`
	AppVersion                  string            `json:"app_version"`
	Endpoint                    string            `json:"endpoint,omitempty"`
	IngestionToken              string            `json:"ingestion_token,omitempty"`
	Consent                     map[Category]bool `json:"consent,omitempty"`
	EssentialOperationsReviewed bool              `json:"essential_operations_reviewed,omitempty"`
	SQLitePath                  string            `json:"sqlite_path,omitempty"`
	IdleThresholdMS             uint64            `json:"idle_threshold_ms,omitempty"`
	BatchSize                   uint64            `json:"batch_size,omitempty"`
	FlushIntervalMS             uint64            `json:"flush_interval_ms,omitempty"`
	Queue                       *QueueConfig      `json:"queue,omitempty"`
	Limits                      *Limits           `json:"limits,omitempty"`
}

type QueueConfig struct {
	MaxEvents            uint64 `json:"max_events,omitempty"`
	MaxBytes             uint64 `json:"max_bytes,omitempty"`
	TTLMS                uint64 `json:"ttl_ms,omitempty"`
	PerCategoryMaxEvents uint64 `json:"per_category_max_events,omitempty"`
}

type Limits struct {
	MaxProperties  uint64 `json:"max_properties,omitempty"`
	MaxStringBytes uint64 `json:"max_string_bytes,omitempty"`
	MaxEventBytes  uint64 `json:"max_event_bytes,omitempty"`
}

// String avoids accidentally logging tokens or local storage paths.
func (c Config) String() string   { return "telemetry.Config{private configuration}" }
func (c Config) GoString() string { return c.String() }

type NativeError struct{ Code int }

func (e NativeError) Error() string {
	return fmt.Sprintf("telemetry native operation failed (status %d)", e.Code)
}

var ErrClosed = errors.New("telemetry SDK is closed")
var ErrCGODisabled = errors.New("telemetry requires CGO_ENABLED=1 and the Rust native library; pure-Go fallback is not implemented")

// Telemetry can be shared by goroutines. Close invalidates the wrapper handle
// immediately; calls already entering native code may finish or return an error.
type Telemetry struct {
	mu     sync.RWMutex
	handle uint64
	closed bool
}

func New(config Config) (*Telemetry, error) {
	bytes, err := json.Marshal(config)
	if err != nil {
		return nil, errors.New("invalid telemetry config")
	}
	handle, err := nativeInit(bytes)
	if err != nil {
		return nil, err
	}
	return &Telemetry{handle: handle}, nil
}

// Command dispatches the canonical engine protocol. Unknown operations/fields
// and unregistered events are rejected by the Rust engine.
func (sdk *Telemetry) Command(command map[string]any) (json.RawMessage, error) {
	bytes, err := json.Marshal(command)
	if err != nil {
		return nil, errors.New("invalid telemetry command")
	}
	sdk.mu.RLock()
	if sdk.closed {
		sdk.mu.RUnlock()
		return nil, ErrClosed
	}
	handle := sdk.handle
	sdk.mu.RUnlock()
	// The C registry clones its SDK before dispatch and safely rejects stale
	// numeric handles, so wrapper locks need not span potentially blocking calls.
	return nativeCommand(handle, bytes)
}

func (sdk *Telemetry) invoke(op string, fields map[string]any) error {
	if fields == nil {
		fields = map[string]any{}
	}
	fields["op"] = op
	_, err := sdk.Command(fields)
	return err
}

func (sdk *Telemetry) SetConsent(category Category, enabled bool) error {
	return sdk.invoke("set_consent", map[string]any{"category": category, "enabled": enabled})
}
func (sdk *Telemetry) Action(id string, result ActionResult) error {
	return sdk.invoke("action", map[string]any{"id": id, "result": result})
}
func (sdk *Telemetry) CaptureError(code, message string) error {
	return sdk.invoke("capture_error", map[string]any{"code": code, "message": message})
}
func (sdk *Telemetry) Increment(name string, amount float64) error {
	return sdk.invoke("increment", map[string]any{"name": name, "amount": amount})
}
func (sdk *Telemetry) Event(name string, properties map[string]any) error {
	if properties == nil {
		properties = map[string]any{}
	}
	return sdk.invoke("event", map[string]any{"name": name, "properties": properties})
}
func (sdk *Telemetry) RegisterEvent(definition map[string]any) error {
	return sdk.invoke("register_event", map[string]any{"definition": definition})
}
func (sdk *Telemetry) ScreenEnter(id string) error {
	return sdk.invoke("screen_enter", map[string]any{"id": id})
}
func (sdk *Telemetry) ScreenLeave(id string) error {
	return sdk.invoke("screen_leave", map[string]any{"id": id})
}
func (sdk *Telemetry) Lifecycle(windowID string, visible, focused bool) error {
	return sdk.invoke("lifecycle", map[string]any{"window_id": windowID, "visible": visible, "focused": focused})
}
func (sdk *Telemetry) Interaction() error { return sdk.invoke("interaction", nil) }
func (sdk *Telemetry) Suspend(suspended bool) error {
	return sdk.invoke("suspend", map[string]any{"suspended": suspended})
}
func (sdk *Telemetry) Checkpoint() error { return sdk.invoke("checkpoint", nil) }

func (sdk *Telemetry) WorkflowStart(name string) (string, error) {
	response, err := sdk.Command(map[string]any{"op": "workflow_start", "name": name})
	if err != nil {
		return "", err
	}
	var result struct {
		ID string `json:"id"`
	}
	if json.Unmarshal(response, &result) != nil {
		return "", errors.New("invalid workflow response")
	}
	return result.ID, nil
}
func (sdk *Telemetry) WorkflowStep(id, step string) error {
	return sdk.invoke("workflow_step", map[string]any{"id": id, "step": step})
}
func (sdk *Telemetry) WorkflowEnd(id string, outcome WorkflowOutcome) error {
	return sdk.invoke("workflow_end", map[string]any{"id": id, "outcome": outcome})
}

type Span struct {
	sdk   *Telemetry
	name  string
	start time.Time
	epoch uint64
	once  sync.Once
	err   error
}

func (sdk *Telemetry) Span(name string) *Span {
	response, err := sdk.Diagnostics()
	if err != nil {
		return &Span{err: err}
	}
	var state struct {
		Consent map[Category]bool `json:"consent"`
		Epoch   uint64            `json:"consent_epoch"`
	}
	if err := json.Unmarshal(response, &state); err != nil {
		return &Span{err: errors.New("invalid telemetry consent state")}
	}
	if !state.Consent[Performance] {
		return &Span{}
	}
	return &Span{sdk: sdk, name: name, start: time.Now(), epoch: state.Epoch}
}
func (span *Span) Finish() error {
	span.once.Do(func() {
		if span.sdk != nil {
			span.err = span.sdk.invoke("span", map[string]any{"name": span.name, "duration_ms": float64(time.Since(span.start).Nanoseconds()) / 1e6, "consent_epoch": span.epoch})
		}
	})
	return span.err
}

func timeoutMS(ctx context.Context) (int64, error) {
	if err := ctx.Err(); err != nil {
		return 0, err
	}
	timeout := 5 * time.Second
	if deadline, ok := ctx.Deadline(); ok {
		timeout = time.Until(deadline)
	}
	if timeout <= 0 {
		return 0, context.DeadlineExceeded
	}
	return timeout.Milliseconds(), nil
}

// Flush blocks the calling goroutine, bounded by ctx's deadline (default 5s).
// Cancellation without a deadline is checked before the native call; it cannot
// interrupt an already running native call. Use a deadline when this matters.
func (sdk *Telemetry) Flush(ctx context.Context) error {
	timeout, err := timeoutMS(ctx)
	if err != nil {
		return err
	}
	return sdk.invoke("flush", map[string]any{"timeout_ms": timeout})
}
func (sdk *Telemetry) Diagnostics() (json.RawMessage, error) {
	return sdk.Command(map[string]any{"op": "diagnostics"})
}
func (sdk *Telemetry) Capabilities() (json.RawMessage, error) {
	return sdk.Command(map[string]any{"op": "capabilities"})
}

// Close always releases the handle, even if the final flush fails. A repeated
// Close is harmless. It returns the shutdown error so callers can report loss.
// It does not wait for concurrent Flush calls before starting its own deadline.
func (sdk *Telemetry) Close(ctx context.Context) error {
	sdk.mu.Lock()
	if sdk.closed {
		sdk.mu.Unlock()
		return nil
	}
	handle := sdk.handle
	sdk.handle = 0
	sdk.closed = true
	sdk.mu.Unlock()
	timeout, err := timeoutMS(ctx)
	if err == nil {
		command, _ := json.Marshal(map[string]any{"op": "shutdown", "timeout_ms": timeout})
		_, err = nativeCommand(handle, command)
	}
	destroyErr := nativeDestroy(handle)
	if err != nil {
		return err
	}
	return destroyErr
}
