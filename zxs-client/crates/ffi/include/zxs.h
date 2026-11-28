#ifndef ZXS_H
#define ZXS_H

#include <stddef.h>
#include <stdbool.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/* The request is UTF-8 JSON. The returned UTF-8 JSON is owned by the caller
 * and must be freed with zxs_free. A null return indicates allocation or
 * serialization failure. The request buffer is used only during the call. */
/* direction is 0 for upload and 1 for download. Progress runs on the calling
 * thread. The callback and context are only used during this call. */
typedef void (*zxs_progress_callback)(void *context, uint8_t direction,
    uint64_t transferred, uint64_t total, uint64_t bytes_per_second);
/* Returns true once the download should stop. Checked on the calling thread.
 * Either callback may be null. A cancelled download returns status 499. */
typedef bool (*zxs_cancel_callback)(void *context);
char *zxs_call_with_transfer(const uint8_t *request, size_t length,
    zxs_progress_callback progress, zxs_cancel_callback cancelled, void *context);
void zxs_free(char *value);

#ifdef __cplusplus
}
#endif
#endif
