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
void zxs_free(char *value);

#ifdef __cplusplus
}
#endif
#endif
