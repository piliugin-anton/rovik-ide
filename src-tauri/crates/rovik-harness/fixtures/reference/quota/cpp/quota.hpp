#ifndef HARNESS_EXAMPLE_QUOTA_HPP
#define HARNESS_EXAMPLE_QUOTA_HPP

#include <cstdint>
#include <expected>

namespace quota_example {

enum class ReserveError { InvalidAmount, Insufficient };

class PositiveUnits {
public:
    static std::expected<PositiveUnits, ReserveError>
    from(std::uint64_t value) noexcept {
        if (value == 0) {
            return std::unexpected(ReserveError::InvalidAmount);
        }
        return PositiveUnits(value);
    }

    std::uint64_t value() const noexcept { return value_; }

private:
    explicit PositiveUnits(std::uint64_t value) noexcept : value_(value) {}
    std::uint64_t value_;
};

class Quota {
public:
    explicit Quota(std::uint64_t remaining) noexcept : remaining_(remaining) {}

    std::uint64_t remaining() const noexcept { return remaining_; }

    std::expected<void, ReserveError> reserve(PositiveUnits amount) noexcept {
        if (amount.value() > remaining_) {
            return std::unexpected(ReserveError::Insufficient);
        }
        remaining_ -= amount.value();
        return {};
    }

private:
    std::uint64_t remaining_;
};

}  // namespace quota_example
#endif
