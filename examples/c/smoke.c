#include "telemetry.h"
#include <assert.h>
#include <pthread.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

static void request(uint64_t handle, const char *json, int expected) {
    TelemetryBuffer output = {0};
    int status = telemetry_v1_command(handle, (const uint8_t *)json, strlen(json), &output);
    assert(status == expected);
    assert(output.data != NULL && output.len > 0);
    /* The response is not terminated, so never pass it directly to strlen. */
    assert(output.data[0] == '{');
    telemetry_v1_buffer_free(output);
}

static void *thread_main(void *argument) {
    uint64_t handle = *(const uint64_t *)argument;
    for (int i = 0; i < 100; i++) request(handle, "{\"op\":\"diagnostics\"}", 0);
    return NULL;
}

int main(void) {
    uint64_t handle = 123;
    assert(telemetry_v1_init(NULL, 1, &handle) == 1 && handle == 0);
    const uint8_t bad_utf8[] = {255};
    assert(telemetry_v1_init(bad_utf8, sizeof bad_utf8, &handle) == 1);
    const char *config = "{\"app_id\":\"c.example\",\"app_version\":\"1.0\"}";
    assert(telemetry_v1_init((const uint8_t *)config, strlen(config), &handle) == 0);
    assert(handle != 0);
    request(handle, "{\"op\":\"capabilities\"}", 0);
    request(handle, "{\"op\":\"action\",\"id\":\"search.submit\",\"result\":\"success\"}", 0);
    request(handle, "{\"op\":\"capture_error\",\"code\":\"network.error\",\"message\":\"password=synthetic\"}", 0);
    request(handle, "{\"op\":\"unknown\"}", 3);
    TelemetryBuffer invalid = {0};
    assert(telemetry_v1_command(handle, bad_utf8, sizeof bad_utf8, &invalid) == 1);
    telemetry_v1_buffer_free(invalid);
    pthread_t workers[4];
    for (int i = 0; i < 4; i++) assert(pthread_create(&workers[i], NULL, thread_main, &handle) == 0);
    for (int i = 0; i < 4; i++) assert(pthread_join(workers[i], NULL) == 0);
    request(handle, "{\"op\":\"shutdown\",\"timeout_ms\":1000}", 0);
    assert(telemetry_v1_destroy(handle) == 0);
    assert(telemetry_v1_destroy(handle) == 2);
    request(handle, "{\"op\":\"diagnostics\"}", 2);
    puts("C ABI smoke passed (UTF-8, ownership, errors, concurrency, stale handles)");
    return 0;
}
