MAX = (1 << 64) - 1


def reserve(remaining: int, amount: int) -> dict:
    if type(remaining) is not int or type(amount) is not int or not 0 <= remaining <= MAX or not 0 <= amount <= MAX:
        raise ValueError("quota values must be unsigned 64-bit integers")
    if amount == 0:
        return {"status": "invalid_amount", "remaining": remaining}
    if amount > remaining:
        return {"status": "insufficient", "remaining": remaining}
    return {"status": "ok", "remaining": remaining - amount}
