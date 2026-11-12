const MAX = (1n << 64n) - 1n;
export interface Reservation {
  status: "ok" | "invalid_amount" | "insufficient";
  remaining: bigint;
}

export function reserve(remaining: bigint, amount: bigint): Reservation {
  if (typeof remaining !== "bigint" || typeof amount !== "bigint" || remaining < 0n || amount < 0n || remaining > MAX || amount > MAX) {
    throw new RangeError("quota values must be unsigned 64-bit integers");
  }
  if (amount === 0n) return { status: "invalid_amount", remaining };
  if (amount > remaining) return { status: "insufficient", remaining };
  return { status: "ok", remaining: remaining - amount };
}
