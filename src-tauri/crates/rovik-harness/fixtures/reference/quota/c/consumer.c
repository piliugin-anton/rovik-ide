#include "/snapshot/implementations/quota/quota.h"
#include <stddef.h>
#include <stdint.h>
#include <string.h>

int main(int argc, char **argv) {
    if (argc != 2) return 2;
    const char *name = argv[1];
    if (!strcmp(name, "null-preserves-output")) {
        uint64_t output = 91;
        quota value;
        return !(quota_init(NULL, 0) == QUOTA_INVALID_ARGUMENT &&
                 quota_reserve(NULL, 1) == QUOTA_INVALID_ARGUMENT &&
                 quota_remaining(NULL, &output) == QUOTA_INVALID_ARGUMENT && output == 91 &&
                 quota_remaining(&value, NULL) == QUOTA_INVALID_ARGUMENT);
    }
    static const struct { const char *name; uint64_t initial, amount, remaining; quota_status status; } cases[] = {
        {"reserve-success", 10, 3, 7, QUOTA_OK},
        {"reserve-exact", 10, 10, 0, QUOTA_OK},
        {"reserve-insufficient", 10, 11, 10, QUOTA_INSUFFICIENT},
        {"reserve-zero", 10, 0, 10, QUOTA_INVALID_AMOUNT},
        {"reserve-empty", 0, 1, 0, QUOTA_INSUFFICIENT},
        {"reserve-maximum", UINT64_MAX, UINT64_MAX, 0, QUOTA_OK},
        {"reserve-insufficient-maximum", 1, UINT64_MAX, 1, QUOTA_INSUFFICIENT}
    };
    for (size_t i = 0; i < sizeof(cases) / sizeof(cases[0]); ++i) {
        if (strcmp(name, cases[i].name)) continue;
        quota value;
        uint64_t observed = 42;
        return !(quota_init(&value, cases[i].initial) == QUOTA_OK &&
                 quota_reserve(&value, cases[i].amount) == cases[i].status &&
                 quota_remaining(&value, &observed) == QUOTA_OK && observed == cases[i].remaining);
    }
    return 2;
}
