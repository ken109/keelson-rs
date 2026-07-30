use std::sync::OnceLock;
use std::sync::atomic::{AtomicI32, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// The uniqueness source: a process-unique, time-derived base plus an atomic
/// counter. Generated factory modules hold one per model:
///
/// ```ignore
/// static SEQ: Sequence = Sequence::new();
/// // …
/// id: self.id.resolve(f, |_| Set::Value(SEQ.next_i64())),
/// ```
///
/// Two promises, in order of strength:
///
/// - **In-process, values never repeat** — the counter is atomic, so
///   `create_many(&db, 100)` (and every factory call across every test in the
///   binary) draws distinct values.
/// - **Across processes, collision is improbable** — the base is taken from
///   the clock at first use (the same shape the Layer 2 spec's `key()`
///   pinned), so runs against a shared persistent server land in different
///   ranges. Improbable, not impossible: this is test-data machinery, not a
///   coordination service.
///
/// Values are positive and fit `i32`, so the one sequence serves `integer`
/// (PostgreSQL/MySQL) and `INTEGER` (SQLite) primary keys alike.
///
/// Deliberately **outside the [`Faker`](crate::Faker) seed**: sequences exist
/// for uniqueness, and a reproducible primary key against a shared server
/// would reproduce a collision (crate docs, "the determinism switch").
#[derive(Debug)]
pub struct Sequence {
    base: OnceLock<i32>,
    next: AtomicI32,
}

impl Sequence {
    /// A fresh sequence; `const`, so it can be a `static` in a generated
    /// module. The base is drawn from the clock on first use, not here.
    pub const fn new() -> Self {
        Sequence {
            base: OnceLock::new(),
            next: AtomicI32::new(0),
        }
    }

    /// The next value, `i32`-typed for dialects whose `integer` is 32-bit.
    pub fn next_i32(&self) -> i32 {
        let base = *self.base.get_or_init(|| {
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos();
            // Positive, below i32::MAX, with 2^16 of counter headroom below
            // the next possible base.
            ((nanos as i64) & 0x3fff_0000) as i32
        });
        base + self.next.fetch_add(1, Ordering::Relaxed)
    }

    /// The next value, widened — the same counter as
    /// [`next_i32`](Sequence::next_i32), for `i64`-typed columns.
    pub fn next_i64(&self) -> i64 {
        i64::from(self.next_i32())
    }
}

impl Default for Sequence {
    fn default() -> Self {
        Sequence::new()
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    #[test]
    fn a_static_sequence_yields_distinct_positive_i32_values() {
        static SEQ: Sequence = Sequence::new();
        let mut seen = HashSet::new();
        for _ in 0..200 {
            let v = SEQ.next_i32();
            assert!(v >= 0);
            assert!(seen.insert(v), "sequence repeated {v}");
        }
    }

    #[test]
    fn the_i64_getter_shares_the_counter_with_the_i32_one() {
        let seq = Sequence::new();
        let a = seq.next_i32();
        let b = seq.next_i64();
        assert_eq!(b, i64::from(a) + 1, "one counter behind both getters");
    }
}
