use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// The seedable random-value source every template's random defaults draw
/// from.
///
/// A SplitMix64 generator (Steele, Lea & Flood's public-domain algorithm),
/// implemented here rather than depended upon — see the crate docs for the
/// `fake`-vs-`rand`-vs-in-crate evaluation. The output sequence for a given
/// seed is **pinned by test in this crate**: two [`Faker::seeded`] instances
/// with the same seed produce identical values, on every platform, forever —
/// that is the determinism switch's whole contract.
///
/// Not cryptographic, and not meant to be: this generates test data.
#[derive(Debug, Clone)]
pub struct Faker {
    state: u64,
}

impl Faker {
    /// A faker whose entire output is determined by `seed` — the
    /// reproducibility switch. Same seed, same values, always.
    pub fn seeded(seed: u64) -> Self {
        Faker { state: seed }
    }

    /// A faker seeded from wall-clock entropy plus a process-global counter —
    /// what `create`/`create_many` use when the caller does not care about
    /// reproducing the run.
    pub fn from_entropy() -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos() as u64;
        let salt = COUNTER
            .fetch_add(1, Ordering::Relaxed)
            .wrapping_mul(0x9E37_79B9_7F4A_7C15);
        Faker::seeded(nanos ^ salt)
    }

    /// The next raw 64-bit value.
    pub fn next_u64(&mut self) -> u64 {
        // SplitMix64.
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// A value in `0..n`. The multiply-shift reduction carries a negligible
    /// modulo bias — acceptable for test data, and deterministic, which is
    /// what matters here.
    ///
    /// # Panics
    ///
    /// If `n` is zero — an empty range has no value to produce.
    pub fn below(&mut self, n: u64) -> u64 {
        assert!(n > 0, "Faker::below(0): empty range");
        ((u128::from(self.next_u64()) * u128::from(n)) >> 64) as u64
    }

    /// A value in `lo..=hi`.
    ///
    /// # Panics
    ///
    /// If `lo > hi`.
    pub fn i64_in(&mut self, lo: i64, hi: i64) -> i64 {
        assert!(lo <= hi, "Faker::i64_in: empty range {lo}..={hi}");
        let span = hi.wrapping_sub(lo) as u64;
        if span == u64::MAX {
            return self.next_u64() as i64;
        }
        lo.wrapping_add(self.below(span + 1) as i64)
    }

    /// A value in `lo..=hi`, `i32`-typed for dialects whose `integer` is
    /// 32-bit.
    ///
    /// # Panics
    ///
    /// If `lo > hi`.
    pub fn i32_in(&mut self, lo: i32, hi: i32) -> i32 {
        self.i64_in(i64::from(lo), i64::from(hi)) as i32
    }

    /// A random lowercase-alphanumeric string of `len` characters —
    /// `"user-{alnum}"`-style default values are built from this.
    pub fn alnum(&mut self, len: usize) -> String {
        const CHARS: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789";
        (0..len)
            .map(|_| CHARS[self.below(CHARS.len() as u64) as usize] as char)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The reproducibility contract, pinned to exact values: this sequence may
    /// never change, on any platform, in any release.
    #[test]
    fn the_seeded_output_sequence_is_pinned() {
        let mut f = Faker::seeded(42);
        assert_eq!(f.next_u64(), 13_679_457_532_755_275_413);
        assert_eq!(f.next_u64(), 2_949_826_092_126_892_291);
        assert_eq!(f.next_u64(), 5_139_283_748_462_763_858);
        assert_eq!(Faker::seeded(7).alnum(8), "oa6uqiql");
    }

    #[test]
    fn same_seed_same_values_different_seed_different_values() {
        let mut a = Faker::seeded(1);
        let mut b = Faker::seeded(1);
        let mut c = Faker::seeded(2);
        for _ in 0..16 {
            assert_eq!(a.next_u64(), b.next_u64());
        }
        assert_ne!(Faker::seeded(1).next_u64(), c.next_u64());
    }

    #[test]
    fn ranged_values_stay_in_range() {
        let mut f = Faker::seeded(3);
        for _ in 0..256 {
            let v = f.i64_in(18, 90);
            assert!((18..=90).contains(&v));
            let v = f.i32_in(-5, 5);
            assert!((-5..=5).contains(&v));
            assert!(f.below(3) < 3);
        }
        // Degenerate and full ranges hold too.
        assert_eq!(f.i64_in(9, 9), 9);
        let _ = f.i64_in(i64::MIN, i64::MAX);
    }

    #[test]
    fn alnum_is_lowercase_alphanumeric_of_the_asked_length() {
        let mut f = Faker::from_entropy();
        let s = f.alnum(32);
        assert_eq!(s.len(), 32);
        assert!(
            s.chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        );
    }

    #[test]
    fn entropy_fakers_differ_even_when_created_back_to_back() {
        // The counter salt makes same-nanosecond construction distinct.
        let mut a = Faker::from_entropy();
        let mut b = Faker::from_entropy();
        assert_ne!(a.next_u64(), b.next_u64());
    }
}
