from __future__ import annotations

import re

MAX_U64 = (1 << 64) - 1
_DECIMAL = re.compile(r"^(0|[1-9][0-9]*)$")


def parse_u64(text: str) -> int:
    if not isinstance(text, str) or not _DECIMAL.fullmatch(text):
        raise ValueError("Expected a canonical unsigned decimal string")
    # Reject oversized input before integer conversion.
    if len(text) > 20:
        raise ValueError("Value exceeds the unsigned 64-bit range")
    value = int(text, 10)
    if value > MAX_U64:
        raise ValueError("Value exceeds the unsigned 64-bit range")
    return value


def reserve_expected(before: str, amount: str) -> dict[str, str]:
    remaining = parse_u64(before)
    requested = parse_u64(amount)
    if requested == 0:
        return {"status": "invalid_amount", "remaining": str(remaining)}
    if requested > remaining:
        return {"status": "insufficient", "remaining": str(remaining)}
    return {"status": "ok", "remaining": str(remaining - requested)}
