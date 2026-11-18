//! A tiny deterministic random source.
//!
//! SplitMix64 with no dependency and no `getrandom`: the whole point of the
//! plugin is that `(seed, archetype, tier)` reproduces the same creature on
//! every machine, in wasm and natively, forever. Anything platform- or
//! time-seeded would break that promise.

/// Deterministic 64-bit generator.
#[derive(Debug, Clone)]
pub struct Rng {
    state: u64,
}

impl Rng {
    /// Seed the stream. `0` is a perfectly good seed — SplitMix64 has no weak
    /// zero state, which is why it is used here instead of a raw LCG.
    pub fn new(seed: u64) -> Self {
        Self {
            state: seed ^ 0x9E37_79B9_7F4A_7C15,
        }
    }

    /// Next 64 bits.
    pub fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// `[0, 1)`.
    pub fn unit(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }

    /// `[lo, hi)`.
    pub fn range(&mut self, lo: f64, hi: f64) -> f64 {
        lo + (hi - lo) * self.unit()
    }

    /// An integer in `[0, n)`; `n == 0` yields `0` rather than dividing by zero.
    pub fn below(&mut self, n: u32) -> u32 {
        if n == 0 {
            return 0;
        }
        (self.next_u64() % u64::from(n)) as u32
    }

    /// `true` with probability `p`.
    pub fn chance(&mut self, p: f64) -> bool {
        self.unit() < p
    }

    /// Pick one element of a non-empty slice.
    pub fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        debug_assert!(!items.is_empty(), "pick() needs a non-empty slice");
        let index = self.below(items.len() as u32) as usize;
        &items[index]
    }
}

/// Fold a seed and some structural inputs into one stream seed, so
/// `(seed, archetype, tier)` cannot collide by accident.
pub fn mix_seed(seed: u64, parts: &[u64]) -> u64 {
    let mut rng = Rng::new(seed);
    let mut out = rng.next_u64();
    for part in parts {
        out = out
            .rotate_left(17)
            .wrapping_mul(0x9E37_79B9_7F4A_7C15)
            .wrapping_add(*part)
            ^ rng.next_u64();
    }
    // A final avalanche, so adjacent seeds give visibly different creatures.
    out ^= out >> 33;
    out = out.wrapping_mul(0xFF51_AFD7_ED55_8CCD);
    out ^= out >> 33;
    out = out.wrapping_mul(0xC4CE_B9FE_1A85_EC53);
    out ^= out >> 33;
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_same_seed_repeats_and_neighbours_differ() {
        let a: Vec<u64> = (0..8).map(|_| Rng::new(7).next_u64()).collect();
        assert!(a.iter().all(|value| *value == a[0]), "deterministic");

        let mut first = Rng::new(1);
        let mut second = Rng::new(2);
        assert_ne!(first.next_u64(), second.next_u64());
    }

    #[test]
    fn ranges_stay_inside_their_bounds() {
        let mut rng = Rng::new(42);
        for _ in 0..5_000 {
            let value = rng.range(-3.5, 9.25);
            assert!(((-3.5)..9.25).contains(&value), "{value}");
            let unit = rng.unit();
            assert!((0.0..1.0).contains(&unit), "{unit}");
            assert!(rng.below(7) < 7);
            assert_eq!(rng.below(0), 0);
        }
    }

    #[test]
    fn mixing_a_seed_is_order_sensitive() {
        assert_ne!(mix_seed(5, &[1, 2]), mix_seed(5, &[2, 1]));
        assert_eq!(mix_seed(5, &[1, 2]), mix_seed(5, &[1, 2]));
    }
}
