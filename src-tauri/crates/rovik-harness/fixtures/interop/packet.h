#ifndef PACKET_H
#define PACKET_H
#include <stddef.h>
#include <stdint.h>
#ifdef __cplusplus
extern "C" {
#endif
typedef struct packet packet;
typedef struct packet_report { uint64_t checksum; size_t size; } packet_report;
typedef void (*packet_callback)(void *, uint64_t);
/* Caller owns the result and releases it once. Failure leaves *out unchanged. */
int packet_create(size_t size, uint64_t seed, packet **out);
/* Callback/context are borrowed only for this call, never retained. */
int packet_read(const packet *, packet_report *, packet_callback, void *);
void packet_destroy(packet *);
size_t packet_report_size(void);
size_t packet_report_size_offset(void);
#ifdef __cplusplus
}
#endif
#endif
