#ifndef TELEMETRY_V1_H
#define TELEMETRY_V1_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/* ABI major 1. Opaque numeric handles never expose Rust memory/layout.
 * Input strings are UTF-8 pointer+length (no terminating NUL required).
 * Inputs must remain readable for the duration of a call; they are not retained.
 * Maximum input size: 1 MiB. All out pointers must be aligned and writable.
 * Status: 0 success, 1 invalid input, 2 invalid handle, 3 SDK failure, 4 panic.
 * Errors never include caller data or secret configuration.
 */
typedef struct TelemetryBuffer {
    uint8_t *data;
    size_t len;
} TelemetryBuffer;

int32_t telemetry_v1_init(const uint8_t *config, size_t config_len,
                          uint64_t *out_handle);

/* Returns JSON in out_buffer, including generic error JSON on nonzero status.
 * The data is not NUL terminated. Consume before releasing it exactly once with
 * telemetry_v1_buffer_free, from the SAME loaded library. Never use free().
 * A null output is rejected; otherwise output initializes to {NULL,0}.
 * A non-null returned buffer must be freed on both success and error.
 */
int32_t telemetry_v1_command(uint64_t handle, const uint8_t *request,
                             size_t request_len, TelemetryBuffer *out_buffer);
void telemetry_v1_buffer_free(TelemetryBuffer buffer);

/* Invalidates handle even if its final best-effort shutdown cannot flush.
 * For guaranteed final export send {"op":"shutdown","timeout_ms":...} first.
 * Handles support concurrent commands. Destroy rejects all later handle lookups;
 * commands already in progress may finish. Coordinate destroy in the adapter.
 * Stale/double destroy returns status 2. Buffer double free is invalid.
 * Do not call from fatal signal handlers. Operations other than collection,
 * notably flush/shutdown and consent withdrawal, may block; run off UI threads.
 */
int32_t telemetry_v1_destroy(uint64_t handle);

#ifdef __cplusplus
}
#endif
#endif
