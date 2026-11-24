//! A tiny deterministic PRNG for procedural props.
//!
//! Why not `rand`: a prop must be reproducible from `(def, seed, params)` — the
//! same chest must come back with the same wood and dents after a restart, and a
//! guest that knows the seed must be able to reason about what it is holding.
//! Why not the metamorphosis guest's `rng.rs`: that lives in a wasm crate this
//! binary does not depend on, and copying 30 lines is cheaper than a shared
//! crate with its own MSRV.
//!
//! xorshift64* — small, fast, good enough for taste, and stable across runs.

/// One generator. `seed` must be non-zero (it is forced non-zero on construction).
#[derive(Debug, Clone)]
pub struct Rng {
    state: u64,
}

impl Rng {
    pub fn new(seed: u64) -> Self {
        // A zero state is a fixed point of xorshift; the constant also keeps
        // seed 0 and seed 1 from producing visibly related props.
        Self {
            state: if seed == 0 { 0x9E37_79B9_7F4A_7C15 } else { seed },
        }
    }

    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.state;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.state = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// Uniform in `0..n` (`0` when `n` is 0, so callers cannot divide by zero).
    pub fn below(&mut self, n: u32) -> u32 {
        if n == 0 {
            0
        } else {
            (self.next_u64() % u64::from(n)) as u32
        }
    }

    /// Uniform in `lo..=hi`. A non-finite or inverted range collapses to `lo`.
    pub fn range(&mut self, lo: f64, hi: f64) -> f64 {
        if !lo.is_finite() || !hi.is_finite() || hi <= lo {
            return if lo.is_finite() { lo } else { 0.0 };
        }
        let unit = (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64;
        lo + unit * (hi - lo)
    }

    /// Uniform integer in `lo..=hi`.
    pub fn int(&mut self, lo: i64, hi: i64) -> i64 {
        if hi <= lo {
            return lo;
        }
        lo + i64::from(self.below((hi - lo + 1) as u32))
    }

    pub fn chance(&mut self, probability: f64) -> bool {
        self.range(0.0, 1.0) < probability.clamp(0.0, 1.0)
    }

    /// Pick one of `items`. An empty slice is a bug; `fallback` keeps it total.
    pub fn pick<'a>(&mut self, items: &'a [&'a str], fallback: &'a str) -> &'a str {
        if items.is_empty() {
            return fallback;
        }
        items[self.below(items.len() as u32) as usize]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_same_seed_replays_exactly() {
        let first: Vec<u64> = {
            let mut rng = Rng::new(42);
            (0..16).map(|_| rng.next_u64()).collect()
        };
        let second: Vec<u64> = {
            let mut rng = Rng::new(42);
            (0..16).map(|_| rng.next_u64()).collect()
        };
        assert_eq!(first, second);
    }

    #[test]
    fn different_seeds_diverge_and_zero_is_not_a_fixed_point() {
        let mut zero = Rng::new(0);
        let mut one = Rng::new(1);
        assert_ne!(zero.next_u64(), one.next_u64());
        assert_ne!(zero.next_u64(), zero.next_u64());
    }

    #[test]
    fn ranges_stay_inside_their_bounds() {
        let mut rng = Rng::new(7);
        for _ in 0..500 {
            let value = rng.range(2.0, 5.0);
            assert!((2.0..=5.0).contains(&value), "{value}");
            let int = rng.int(-3, 3);
            assert!((-3..=3).contains(&int), "{int}");
            let below = rng.below(3);
            assert!(below < 3);
        }
        // Degenerate ranges must not panic or produce NaN.
        assert_eq!(rng.range(4.0, 4.0), 4.0);
        assert_eq!(rng.range(f64::NAN, 2.0), 0.0);
        assert_eq!(rng.below(0), 0);
    }
}
