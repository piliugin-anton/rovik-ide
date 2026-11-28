#ifndef HARNESS_EXAMPLE_QUOTA_H
#define HARNESS_EXAMPLE_QUOTA_H

#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef struct quota {
    uint64_t remaining;
} quota;

typedef enum quota_status {
    QUOTA_OK = 0,
    QUOTA_INVALID_ARGUMENT = 1,
    QUOTA_INVALID_AMOUNT = 2,
    QUOTA_INSUFFICIENT = 3
} quota_status;

/* Non-null pointers must reference live objects with appropriate access. */
quota_status quota_init(quota *self, uint64_t remaining);

/* Caller serializes access to the same object. Failure preserves state. */
quota_status quota_reserve(quota *self, uint64_t amount);

/* On invalid arguments, does not write to out_remaining. */
quota_status quota_remaining(const quota *self, uint64_t *out_remaining);

#ifdef __cplusplus
}
#endif

#endif
