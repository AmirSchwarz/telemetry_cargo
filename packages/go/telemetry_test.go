//go:build cgo

package telemetry

import (
	"context"
	"encoding/json"
	"net"
	"sync"
	"testing"
	"time"
)

func TestNativePrivacyAndOwnership(t *testing.T) {
	sdk, err := New(Config{AppID: "go.test", AppVersion: "1.0"})
	if err != nil {
		t.Fatal(err)
	}
	if err = sdk.Action("search.submit", Success); err != nil {
		t.Fatal(err)
	}
	if err = sdk.CaptureError("io.error", "password=synthetic"); err != nil {
		t.Fatal(err)
	}
	response, err := sdk.Diagnostics()
	if err != nil || !json.Valid(response) {
		t.Fatalf("diagnostics: %s %v", response, err)
	}
	var workers sync.WaitGroup
	for i := 0; i < 4; i++ {
		workers.Add(1)
		go func() {
			defer workers.Done()
			for j := 0; j < 50; j++ {
				if _, err := sdk.Capabilities(); err != nil {
					t.Error(err)
				}
			}
		}()
	}
	workers.Wait()
	if err = sdk.Close(context.Background()); err != nil {
		t.Fatal(err)
	}
	if err = sdk.Close(context.Background()); err != nil {
		t.Fatal(err)
	}
	if _, err = sdk.Diagnostics(); err != ErrClosed {
		t.Fatalf("closed SDK error = %v", err)
	}
}

func TestInvalidPayloadRejected(t *testing.T) {
	sdk, err := New(Config{AppID: "go.test", AppVersion: "1.0"})
	if err != nil {
		t.Fatal(err)
	}
	defer sdk.Close(context.Background())
	if _, err = sdk.Command(map[string]any{"op": "unknown", "secret": "synthetic"}); err == nil {
		t.Fatal("accepted invalid operation")
	}
}

func TestSpansRespectConsentEpoch(t *testing.T) {
	sdk, err := New(Config{AppID: "go.span", AppVersion: "1.0"})
	if err != nil {
		t.Fatal(err)
	}
	defer sdk.Close(context.Background())
	disabled := sdk.Span("operation.disabled")
	if disabled.sdk != nil || disabled.name != "" || !disabled.start.IsZero() {
		t.Fatal("disabled span retained state")
	}
	if err := sdk.SetConsent(Performance, true); err != nil {
		t.Fatal(err)
	}
	active := sdk.Span("operation.old")
	if err := sdk.SetConsent(Performance, false); err != nil {
		t.Fatal(err)
	}
	if err := sdk.SetConsent(Performance, true); err != nil {
		t.Fatal(err)
	}
	if err := disabled.Finish(); err != nil {
		t.Fatal(err)
	}
	if err := active.Finish(); err != nil {
		t.Fatal(err)
	}
	// No exporter configured: success proves the stale spans queued no data.
	if err := sdk.Flush(context.Background()); err != nil {
		t.Fatal(err)
	}
}

func TestCloseDeadlineDoesNotWaitForConcurrentFlush(t *testing.T) {
	// Accept TCP but deliberately never answer the TLS handshake. The exporter
	// must remain blocked while Close attempts a much shorter deadline.
	listener, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatal(err)
	}
	defer listener.Close()
	accepted := make(chan net.Conn, 1)
	go func() {
		connection, err := listener.Accept()
		if err == nil {
			accepted <- connection
		}
	}()
	sdk, err := New(Config{
		AppID: "go.close.deadline", AppVersion: "1.0",
		Endpoint: "https://" + listener.Addr().String(),
		Consent:  map[Category]bool{UsageAggregate: true},
		// Prevent an automatic export: the accepted connection proves Flush
		// has entered the native exporter before Close is called.
		FlushIntervalMS: 600_000,
	})
	if err != nil {
		t.Fatal(err)
	}
	defer sdk.Close(context.Background())
	if err := sdk.Action("search.submit", Success); err != nil {
		t.Fatal(err)
	}
	flushContext, cancelFlush := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancelFlush()
	flushed := make(chan error, 1)
	go func() { flushed <- sdk.Flush(flushContext) }()
	select {
	case connection := <-accepted:
		defer connection.Close()
	case <-time.After(2 * time.Second):
		t.Fatal("native exporter did not connect")
	}
	closeContext, cancelClose := context.WithTimeout(context.Background(), 100*time.Millisecond)
	defer cancelClose()
	started := time.Now()
	err = sdk.Close(closeContext)
	if elapsed := time.Since(started); elapsed > 500*time.Millisecond {
		t.Fatalf("Close waited %v behind an active Flush", elapsed)
	}
	if err == nil {
		t.Fatal("Close reported successful export for a stalled connection")
	}
	if _, err := sdk.Diagnostics(); err != ErrClosed {
		t.Fatalf("closed SDK error = %v", err)
	}
	select {
	case <-flushed:
	case <-time.After(time.Second):
		t.Fatal("concurrent Flush did not observe shutdown")
	}
}
