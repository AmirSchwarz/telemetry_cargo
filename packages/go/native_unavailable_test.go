//go:build !cgo

package telemetry

import "testing"

func TestExplicitUnsupportedWithoutCGO(t *testing.T) {
	if _, err := New(Config{AppID: "go.test", AppVersion: "1.0"}); err != ErrCGODisabled {
		t.Fatalf("got %v", err)
	}
}
