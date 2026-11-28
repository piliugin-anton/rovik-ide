#include "/snapshot/implementations/quota/quota.hpp"
#include <cstdint>
#include <cstring>
#include <type_traits>
using namespace quota_example;
static_assert(noexcept(PositiveUnits::from(1)));
static_assert(noexcept(Quota(1).reserve(*PositiveUnits::from(1))));
static_assert(!std::is_default_constructible_v<PositiveUnits>);
int main(int argc, char **argv) {
    if (argc != 2) return 2;
    struct Case { const char *name; std::uint64_t initial, amount, remaining; bool success; };
    constexpr Case cases[] = {
        {"reserve-success",10,3,7,true}, {"reserve-exact",10,10,0,true},
        {"reserve-insufficient",10,11,10,false}, {"reserve-zero",10,0,10,false},
        {"reserve-empty",0,1,0,false}, {"reserve-maximum",UINT64_MAX,UINT64_MAX,0,true},
        {"reserve-insufficient-maximum",1,UINT64_MAX,1,false}
    };
    for (const auto &test : cases) {
        if (std::strcmp(argv[1], test.name)) continue;
        Quota value(test.initial);
        auto amount = PositiveUnits::from(test.amount);
        if (!amount) return !(test.amount == 0 && amount.error() == ReserveError::InvalidAmount && value.remaining() == test.remaining);
        auto result = value.reserve(*amount);
        return !(result.has_value() == test.success && value.remaining() == test.remaining &&
                 (result || result.error() == ReserveError::Insufficient));
    }
    return 2;
}
