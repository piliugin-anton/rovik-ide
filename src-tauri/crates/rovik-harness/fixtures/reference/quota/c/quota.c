#include "quota.h"

#include <stddef.h>

quota_status quota_init(quota *self, uint64_t remaining) {
    if (self == NULL) {
        return QUOTA_INVALID_ARGUMENT;
    }
    self->remaining = remaining;
    return QUOTA_OK;
}

quota_status quota_reserve(quota *self, uint64_t amount) {
    if (self == NULL) {
        return QUOTA_INVALID_ARGUMENT;
    }
    if (amount == 0) {
        return QUOTA_INVALID_AMOUNT;
    }
    if (amount > self->remaining) {
        return QUOTA_INSUFFICIENT;
    }
    self->remaining -= amount;
    return QUOTA_OK;
}

quota_status quota_remaining(const quota *self, uint64_t *out_remaining) {
    if (self == NULL || out_remaining == NULL) {
        return QUOTA_INVALID_ARGUMENT;
    }
    *out_remaining = self->remaining;
    return QUOTA_OK;
}
