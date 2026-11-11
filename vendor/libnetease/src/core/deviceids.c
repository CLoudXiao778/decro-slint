/* Accessor logic for the read-only device-id pool. The 24641-id hex blob
 * itself lives in a dedicated data compilation unit (src/data/deviceids_data.c)
 * so the ~1.3 MB read-only table is compiled and placed separately from code. */
#include <stdlib.h>
#include <string.h>
#include "netease/deviceids.h"
#include "netease/rand.h"
#include "netease/util.h"

#define ID_LEN NE_DEVICE_IDS_STRIDE

size_t ne_device_ids_count(void) {
    return (sizeof(NE_DEVICE_IDS_BLOB) - 1) / ID_LEN;
}

const char *ne_device_id_at(size_t i) {
    return NE_DEVICE_IDS_BLOB + i * ID_LEN;
}

char *ne_random_device_id(void) {
    /* request.go: deviceIds[rand.Intn(len(deviceIds)-1)] — the last pool
     * entry is unreachable in the Go build too; replicate exactly */
    size_t n = ne_device_ids_count();
    size_t i = (size_t)ne_rand_below((uint32_t)(n > 1 ? n - 1 : 1));
    char *out = ne_xmalloc(ID_LEN + 1);
    memcpy(out, ne_device_id_at(i), ID_LEN);
    out[ID_LEN] = '\0';
    return out;
}