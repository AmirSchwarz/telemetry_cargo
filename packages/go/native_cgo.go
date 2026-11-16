//go:build cgo

package telemetry

/*
#cgo CFLAGS: -I${SRCDIR}/../../include
#cgo LDFLAGS: -L${SRCDIR}/../../target/release -ltelemetry_ffi
#include <stdlib.h>
#include "telemetry.h"
*/
import "C"

import (
	"encoding/json"
	"errors"
	"unsafe"
)

func nativeInit(config []byte) (uint64, error) {
	input := C.CBytes(config)
	defer C.free(input)
	var handle C.uint64_t
	status := C.telemetry_v1_init((*C.uint8_t)(input), C.size_t(len(config)), &handle)
	if status != 0 {
		return 0, NativeError{int(status)}
	}
	return uint64(handle), nil
}

func nativeCommand(handle uint64, command []byte) (json.RawMessage, error) {
	input := C.CBytes(command)
	defer C.free(input)
	var output C.TelemetryBuffer
	status := C.telemetry_v1_command(C.uint64_t(handle), (*C.uint8_t)(input), C.size_t(len(command)), &output)
	defer C.telemetry_v1_buffer_free(output)
	if status != 0 {
		return nil, NativeError{int(status)}
	}
	if output.data == nil || uint64(output.len) > 1<<30 {
		return nil, errors.New("invalid telemetry response buffer")
	}
	return json.RawMessage(C.GoBytes(unsafe.Pointer(output.data), C.int(output.len))), nil
}

func nativeDestroy(handle uint64) error {
	status := C.telemetry_v1_destroy(C.uint64_t(handle))
	if status != 0 {
		return NativeError{int(status)}
	}
	return nil
}
