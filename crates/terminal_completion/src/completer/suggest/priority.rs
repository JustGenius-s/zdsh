//! This module contains the `Priority` struct, which may be specified on [`Suggestion`]s to
//! influence the order of suggestions returned to users.

use std::cmp::Ordering;

use serde::{Deserialize, Serialize};

/// The lowest number we can represent for a Priority.
const MIN_PRIORITY: i32 = -100;
/// We default to this Priority value if it is not otherwise provided.
const DEFAULT_PRIORITY: i32 = 0;
/// The highest number we can represent for a Priority.
const MAX_PRIORITY: i32 = 100;

/// Priority is part of how we rank completion suggestions. For non-default priority values, we
/// break ties with lexicographic ordering. Higher values are higher priority and appear earlier in
/// lists of suggestsions.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Priority(i32);

impl Priority {
    /// Creates a new Priority with its value clamped to the range [-100, 100].
    pub fn new(value: i32) -> Self {
        Self(value.clamp(MIN_PRIORITY, MAX_PRIORITY))
    }

    pub fn value(&self) -> i32 {
        self.0
    }

    pub fn min() -> Self {
        Self::new(MIN_PRIORITY)
    }

    pub fn max() -> Self {
        Self::new(MAX_PRIORITY)
    }
}

impl Default for Priority {
    fn default() -> Self {
        Self::new(DEFAULT_PRIORITY)
    }
}

impl Ord for Priority {
    fn cmp(&self, other: &Self) -> Ordering {
        self.0.cmp(&other.0)
    }
}

impl PartialOrd for Priority {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl From<crate::catalog::Priority> for Priority {
    fn from(value: crate::catalog::Priority) -> Self {
        match value {
            crate::catalog::Priority::Global(importance)
            | crate::catalog::Priority::Local(importance) => match importance {
                crate::catalog::Importance::More(order) => Self::new(order.0 as i32),
                crate::catalog::Importance::Less(order) => Self::new(-(101 - order.0 as i32)),
            },
            crate::catalog::Priority::Default => Priority::default(),
        }
    }
}

impl From<Priority> for crate::catalog::Priority {
    fn from(value: Priority) -> Self {
        match value.cmp(&Priority::default()) {
            Ordering::Less => crate::catalog::Priority::Global(crate::catalog::Importance::Less(
                crate::catalog::Order(101 - value.value().unsigned_abs()),
            )),
            Ordering::Equal => crate::catalog::Priority::default(),
            Ordering::Greater => {
                crate::catalog::Priority::Global(crate::catalog::Importance::More(
                    crate::catalog::Order(value.value().unsigned_abs()),
                ))
            }
        }
    }
}
