//! CPython-compatible Mersenne Twister (MT19937) — a bit-exact port of the
//! relevant parts of CPython's `_randommodule.c` and `Lib/random.py`.
//!
//! Faithful port of the `random.Random` behaviour world.py relies on
//! (PORT_PLAN.md §4.4, subsystem map "World state model" — DETERMINISTIC DICE):
//!
//! - `init_genrand` / `init_by_array` seeding (used for new campaigns).
//! - `genrand_uint32` core generator.
//! - `getstate` / `setstate` using the exact CPython layout
//!   `(version=3, internal=[624 state words + index pos], gauss_next)`.
//! - `getrandbits(k)` and bounded `randint(a, b)` via CPython's
//!   `_randbelow_with_getrandbits` rejection sampling.
//!
//! Seeding an int (`random.Random(seed)` for a Python `int`) mirrors
//! CPython's `random_seed`: the absolute value of the int is split into
//! little-endian 32-bit words and fed to `init_by_array`. For new GM-Lab
//! campaigns the seed is a 64-bit int, so it becomes one or two 32-bit words.

const N: usize = 624;
const M: usize = 397;
const MATRIX_A: u32 = 0x9908_b0df;
const UPPER_MASK: u32 = 0x8000_0000;
const LOWER_MASK: u32 = 0x7fff_ffff;

/// A bit-exact CPython `random.Random` MT19937 core.
#[derive(Clone, Debug)]
pub struct MersenneTwister {
    mt: [u32; N],
    /// CPython stores `index` (named `pos`/`mti` in places); when `index == N`
    /// the generator reloads on the next draw. `getstate` exposes this as the
    /// 625th element of the internal vector.
    index: usize,
    /// Cached gaussian value from `gauss`/`normalvariate`. Persisted as the
    /// third element of `getstate()`. GM-Lab never uses gauss, so it stays
    /// `None`, but we round-trip it for faithful save/restore.
    gauss_next: Option<f64>,
}

impl MersenneTwister {
    /// `init_genrand(s)` — CPython `init_genrand` in `_randommodule.c`.
    fn init_genrand(&mut self, s: u32) {
        self.mt[0] = s;
        for i in 1..N {
            // mt[i] = 1812433253 * (mt[i-1] ^ (mt[i-1] >> 30)) + i
            let prev = self.mt[i - 1];
            self.mt[i] = (1_812_433_253u32)
                .wrapping_mul(prev ^ (prev >> 30))
                .wrapping_add(i as u32);
        }
        self.index = N;
    }

    /// `init_by_array(init_key)` — CPython `init_by_array` in `_randommodule.c`.
    fn init_by_array(&mut self, init_key: &[u32]) {
        self.init_genrand(19_650_218);
        let key_length = init_key.len();
        let mut i: usize = 1;
        let mut j: usize = 0;
        let mut k: usize = if N > key_length { N } else { key_length };
        while k > 0 {
            let prev = self.mt[i - 1];
            // mt[i] = (mt[i] ^ ((mt[i-1] ^ (mt[i-1] >> 30)) * 1664525)) + init_key[j] + j
            self.mt[i] = (self.mt[i] ^ (prev ^ (prev >> 30)).wrapping_mul(1_664_525))
                .wrapping_add(init_key[j])
                .wrapping_add(j as u32);
            i += 1;
            j += 1;
            if i >= N {
                self.mt[0] = self.mt[N - 1];
                i = 1;
            }
            if j >= key_length {
                j = 0;
            }
            k -= 1;
        }
        k = N - 1;
        while k > 0 {
            let prev = self.mt[i - 1];
            // mt[i] = (mt[i] ^ ((mt[i-1] ^ (mt[i-1] >> 30)) * 1566083941)) - i
            self.mt[i] = (self.mt[i] ^ (prev ^ (prev >> 30)).wrapping_mul(1_566_083_941))
                .wrapping_sub(i as u32);
            i += 1;
            if i >= N {
                self.mt[0] = self.mt[N - 1];
                i = 1;
            }
            k -= 1;
        }
        self.mt[0] = 0x8000_0000; // MSB is 1; assuring non-zero initial array
    }

    /// `genrand_uint32` — CPython core generator producing the next 32-bit word.
    fn genrand_uint32(&mut self) -> u32 {
        if self.index >= N {
            // generate N words at one time
            let mag01 = [0u32, MATRIX_A];
            let mt = &mut self.mt;
            let mut kk = 0usize;
            while kk < N - M {
                let y = (mt[kk] & UPPER_MASK) | (mt[kk + 1] & LOWER_MASK);
                mt[kk] = mt[kk + M] ^ (y >> 1) ^ mag01[(y & 0x1) as usize];
                kk += 1;
            }
            while kk < N - 1 {
                let y = (mt[kk] & UPPER_MASK) | (mt[kk + 1] & LOWER_MASK);
                mt[kk] = mt[kk + M - N] ^ (y >> 1) ^ mag01[(y & 0x1) as usize];
                kk += 1;
            }
            let y = (mt[N - 1] & UPPER_MASK) | (mt[0] & LOWER_MASK);
            mt[N - 1] = mt[M - 1] ^ (y >> 1) ^ mag01[(y & 0x1) as usize];
            self.index = 0;
        }

        let mut y = self.mt[self.index];
        self.index += 1;
        // Tempering
        y ^= y >> 11;
        y ^= (y << 7) & 0x9d2c_5680;
        y ^= (y << 15) & 0xefc6_0000;
        y ^= y >> 18;
        y
    }

    /// Construct from a Python `int` seed exactly as `random.Random(seed)` does.
    ///
    /// CPython's `random_seed` takes `abs(n)`, splits it into little-endian
    /// 32-bit words (at least one word, so 0 -> `[0]`), and calls
    /// `init_by_array`. We accept a `u128` so the 64-bit campaign seeds (and
    /// the occasional larger fixture seed) fit.
    pub fn from_u128_seed(seed: u128) -> Self {
        let mut rng = MersenneTwister {
            mt: [0u32; N],
            index: N,
            gauss_next: None,
        };
        let key = int_to_key(seed);
        rng.init_by_array(&key);
        rng
    }

    /// `getstate()` -> `(version=3, internal[625], gauss_next)`.
    ///
    /// The internal vector is the 624 state words followed by the index `pos`.
    pub fn getstate(&self) -> RngState {
        let mut internal = Vec::with_capacity(N + 1);
        for &word in self.mt.iter() {
            internal.push(word as u64);
        }
        internal.push(self.index as u64);
        RngState {
            version: 3,
            internal,
            gauss: self.gauss_next,
        }
    }

    /// `setstate(state)` — restore from a `getstate()` payload. Mirrors
    /// CPython `random_setstate`: 624 state words + index in `[0, N]`.
    pub fn setstate(&mut self, state: &RngState) -> Result<(), String> {
        if state.internal.len() != N + 1 {
            return Err(format!(
                "rng internal vector must have {} elements, got {}",
                N + 1,
                state.internal.len()
            ));
        }
        for i in 0..N {
            self.mt[i] = state.internal[i] as u32;
        }
        let index = state.internal[N];
        if index > N as u64 {
            return Err(format!("rng index out of range: {index}"));
        }
        self.index = index as usize;
        self.gauss_next = state.gauss;
        Ok(())
    }

    /// `getrandbits(k)` — CPython `random_getrandbits`, k in `0..=...`.
    ///
    /// For `k <= 32` returns the top `k` bits of one 32-bit word. For larger
    /// `k`, assembles little-endian 32-bit words, masking the final word.
    pub fn getrandbits(&mut self, k: u32) -> u128 {
        if k == 0 {
            return 0;
        }
        if k <= 32 {
            return (self.genrand_uint32() >> (32 - k)) as u128;
        }
        let mut result: u128 = 0;
        let mut shift: u32 = 0;
        let mut remaining = k;
        while remaining > 0 {
            let take = remaining.min(32);
            let word = self.genrand_uint32() >> (32 - take);
            result |= (word as u128) << shift;
            shift += 32;
            remaining -= take;
        }
        result
    }

    /// `_randbelow_with_getrandbits(n)` — CPython `Random._randbelow`.
    ///
    /// Returns a value in `[0, n)`. `k = n.bit_length()`; loop `getrandbits(k)`
    /// until `< n` (rejection sampling). `n == 0` returns 0 (CPython guards
    /// against this in callers, but match the safe behaviour).
    pub fn randbelow(&mut self, n: u128) -> u128 {
        if n == 0 {
            return 0;
        }
        let k = bit_length(n);
        let mut r = self.getrandbits(k);
        while r >= n {
            r = self.getrandbits(k);
        }
        r
    }

    /// `randint(a, b)` — inclusive on both ends, via `randrange(a, b+1)`.
    ///
    /// `randrange(start, stop)` = `start + _randbelow(stop - start)`.
    pub fn randint(&mut self, a: i64, b: i64) -> i64 {
        // width = b - a + 1 ; result = a + randbelow(width)
        let width = (b - a + 1) as i128;
        if width <= 0 {
            // CPython would raise; world.py never calls with b < a.
            return a;
        }
        let off = self.randbelow(width as u128) as i128;
        (a as i128 + off) as i64
    }
}

/// Persisted RNG state mirroring `random.Random.getstate()` and
/// `dialog_store._rng_state_to_payload`: `{version, internal[list[int]], gauss}`.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct RngState {
    pub version: u32,
    pub internal: Vec<u64>,
    pub gauss: Option<f64>,
}

/// Number of significant bits in `n` (Python `int.bit_length()`).
fn bit_length(n: u128) -> u32 {
    if n == 0 {
        0
    } else {
        128 - n.leading_zeros()
    }
}

/// Split a non-negative integer into little-endian 32-bit words exactly as
/// CPython's `_PyLong_AsByteArray`-fed `init_by_array` path does. A zero seed
/// produces a single `[0]` word (CPython uses `keymax = 1` when `bits == 0`).
fn int_to_key(seed: u128) -> Vec<u32> {
    if seed == 0 {
        return vec![0];
    }
    let mut key = Vec::new();
    let mut s = seed;
    while s > 0 {
        key.push((s & 0xffff_ffff) as u32);
        s >>= 32;
    }
    key
}
