#include "packet.h"
#include <stdlib.h>
struct packet { unsigned char *first; unsigned char *second; size_t size; uint64_t seed; };
int packet_create(size_t size, uint64_t seed, packet **out) {
    if (!out || !size || size > 1024) return 1;
    packet *value = malloc(sizeof(*value));
    if (!value) return 2;
    value->first = malloc(size);
    if (!value->first) { free(value); return 2; }
    value->second = malloc(size);
    if (!value->second) { free(value->first); free(value); return 2; }
    value->size = size;
    value->seed = seed;
    for (size_t i = 0; i < size; ++i) { value->first[i] = (unsigned char)seed; value->second[i] = (unsigned char)(seed + 1); }
    *out = value;
    return 0;
}
int packet_read(const packet *value, packet_report *out, packet_callback callback, void *context) {
    if (!value || !out) return 1;
    uint64_t checksum = 0;
    for (size_t i = 0; i < value->size; ++i) checksum += value->first[i] + value->second[i];
    out->checksum = checksum; out->size = value->size;
    if (callback) callback(context, checksum);
    return 0;
}
void packet_destroy(packet *value) {
    if (!value) return;
    free(value->second); free(value->first); free(value);
}
size_t packet_report_size(void) { return sizeof(packet_report); }
size_t packet_report_size_offset(void) { return offsetof(packet_report, size); }
