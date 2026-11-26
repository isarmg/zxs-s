#ifndef ZXS_H
#define ZXS_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/* The request is UTF-8 JSON. The returned UTF-8 JSON is owned by the caller
 * and must be freed with zxs_free. A null return indicates allocation or
 * serialization failure. Neither function retains the request buffer. */
char *zxs_call(const uint8_t *request, size_t length);
/* direction is 0 for upload and 1 for download. Progress runs on the calling
 * thread. The callback and context are only used during this call. */
typedef void (*zxs_progress_callback)(void *context, uint8_t direction,
    uint64_t transferred, uint64_t total, uint64_t bytes_per_second);
char *zxs_call_with_progress(const uint8_t *request, size_t length,
    zxs_progress_callback progress, void *context);
void zxs_free(char *value);

#ifdef __cplusplus
}
#endif
#endif
