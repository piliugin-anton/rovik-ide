#include "packet.h"
#include <assert.h>
#include <stdint.h>
#ifdef __cplusplus
extern "C" {
#endif
void harness_reset(int);
int harness_live(void);
#ifdef __cplusplus
}
#endif
static void observed(void *context, uint64_t checksum) { *(uint64_t *)context += checksum; }
int main(void) {
    assert(packet_report_size() == sizeof(packet_report));
    assert(packet_report_size_offset() == offsetof(packet_report, size));
    for (int failure = 1; failure <= 3; ++failure) {
        harness_reset(failure);
        packet *value = (packet *)(uintptr_t)1;
        assert(packet_create(4, 3, &value) == 2);
        assert(value == (packet *)(uintptr_t)1);
        assert(harness_live() == 0);
    }
    harness_reset(0);
    packet *value = NULL;
    assert(packet_create(4, 3, &value) == 0 && value);
    assert(harness_live() == 3);
    uint64_t callback = 0;
    packet_report report = {0,0};
    assert(packet_read(value, &report, observed, &callback) == 0);
    assert(report.size == 4 && report.checksum == 28 && callback == 28);
    assert(packet_read(NULL, &report, observed, &callback) == 1);
    assert(report.size == 4 && report.checksum == 28 && callback == 28);
    assert(packet_create(0, 0, &value) == 1);
    packet_destroy(value);
    assert(harness_live() == 0 && callback == 28);
    packet_destroy(NULL);
    return 0;
}
