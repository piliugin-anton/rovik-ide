#include <stdlib.h>
static int allocation, failure, live;
void harness_reset(int at) { allocation = 0; failure = at; }
int harness_live(void) { return live; }
void *harness_malloc(size_t size) {
    if (++allocation == failure) return NULL;
    void *value = malloc(size);
    if (value) ++live;
    return value;
}
void harness_free(void *value) { if (value) --live; free(value); }
