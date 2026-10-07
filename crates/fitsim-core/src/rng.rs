//! Tiny deterministic PRNG (xorshift32). Seeded from the hardware RNG on the device and with a
//! fixed seed in tests so generator behaviour is reproducible.

#[derive(Clone, Debug)]
pub struct Rng {
    state: u32,
}

impl Rng {
    pub fn new(seed: u32) -> Self {
        // xorshift must never be seeded with zero.
        Self {
            state: if seed == 0 { 0x9E37_79B9 } else { seed },
        }
    }

    pub fn next_u32(&mut self) -> u32 {
        let mut x = self.state;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.state = x;
        x
    }

    /// Uniform in `[0, 1)`.
    pub fn unit(&mut self) -> f32 {
        (self.next_u32() >> 8) as f32 / (1u32 << 24) as f32
    }

    /// Uniform in `[-1, 1)`.
    pub fn signed(&mut self) -> f32 {
        self.unit() * 2.0 - 1.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deterministic_and_in_range() {
        let mut a = Rng::new(42);
        let mut b = Rng::new(42);
        for _ in 0..1000 {
            let v = a.unit();
            assert_eq!(v, b.unit());
            assert!((0.0..1.0).contains(&v));
        }
    }

    #[test]
    fn zero_seed_is_usable() {
        let mut r = Rng::new(0);
        assert_ne!(r.next_u32(), 0);
    }
}
