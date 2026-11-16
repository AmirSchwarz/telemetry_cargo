//go:build !cgo

package telemetry

import "encoding/json"

func nativeInit([]byte) (uint64, error)                     { return 0, ErrCGODisabled }
func nativeCommand(uint64, []byte) (json.RawMessage, error) { return nil, ErrCGODisabled }
func nativeDestroy(uint64) error                            { return ErrCGODisabled }
