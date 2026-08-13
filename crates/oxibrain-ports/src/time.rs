//! Explicit time. Signatures use `Timestamp`, never a bare `i64`.
//! Open intervals use sentinels, never NULL (ARCHITECTURE.md §6.2).

use serde::{Deserialize, Serialize};
use std::fmt;

/// Unix milliseconds, UTC. The only time type in the codebase.
#[derive(Copy, Clone, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
pub struct Timestamp(pub i64);

/// Sentinel for "the beginning of time". `i64::MIN + 1` — never NULL.
pub const TIME_MIN: Timestamp = Timestamp(i64::MIN + 1);
/// Sentinel for "the end of time" / "still true". `i64::MAX - 1` — never NULL.
pub const TIME_MAX: Timestamp = Timestamp(i64::MAX - 1);

impl Timestamp {
    pub const fn from_millis(m: i64) -> Self {
        Self(m)
    }
    pub const fn millis(self) -> i64 {
        self.0
    }
    pub const fn is_min(self) -> bool {
        self.0 == TIME_MIN.0
    }
    pub const fn is_max(self) -> bool {
        self.0 == TIME_MAX.0
    }
}

impl fmt::Debug for Timestamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            TIME_MIN => write!(f, "Timestamp(TIME_MIN)"),
            TIME_MAX => write!(f, "Timestamp(TIME_MAX)"),
            _ => write!(f, "Timestamp({})", self.0),
        }
    }
}

impl fmt::Display for Timestamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.is_min() {
            return write!(f, "-infinity");
        }
        if self.is_max() {
            return write!(f, "+infinity");
        }
        write!(f, "{}", self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sentinels_order_correctly() {
        assert!(TIME_MIN < Timestamp(0));
        assert!(Timestamp(0) < TIME_MAX);
        assert!(TIME_MIN < TIME_MAX);
    }

    #[test]
    fn sentinels_are_not_i64_extrema() {
        assert_ne!(TIME_MIN.0, i64::MIN); // MIN is reserved for NULL-encoding detection
        assert_ne!(TIME_MAX.0, i64::MAX);
    }
}
