use std::num::NonZeroU64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReserveError {
    InvalidAmount,
    Insufficient,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Amount(NonZeroU64);

impl Amount {
    pub fn new(value: u64) -> Result<Self, ReserveError> {
        NonZeroU64::new(value)
            .map(Self)
            .ok_or(ReserveError::InvalidAmount)
    }

    pub fn get(self) -> u64 {
        self.0.get()
    }
}

#[derive(Debug)]
pub struct Quota {
    remaining: u64,
}

impl Quota {
    pub fn new(remaining: u64) -> Self {
        Self { remaining }
    }

    pub fn remaining(&self) -> u64 {
        self.remaining
    }

    pub fn reserve(&mut self, amount: Amount) -> Result<(), ReserveError> {
        let requested = amount.get();
        if requested > self.remaining {
            return Err(ReserveError::Insufficient);
        }
        self.remaining -= requested;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn supplied_contract_cases() {
        let cases = [
            (7, 3, Ok(()), 4),
            (7, 7, Ok(()), 0),
            (7, 8, Err(ReserveError::Insufficient), 7),
            (7, 0, Err(ReserveError::InvalidAmount), 7),
            (0, 1, Err(ReserveError::Insufficient), 0),
            (u64::MAX, u64::MAX, Ok(()), 0),
            (1, u64::MAX, Err(ReserveError::Insufficient), 1),
        ];
        for (before, raw_amount, expected, after) in cases {
            let mut quota = Quota::new(before);
            let actual = Amount::new(raw_amount).and_then(|a| quota.reserve(a));
            assert_eq!(actual, expected);
            assert_eq!(quota.remaining(), after);
        }
    }
}
