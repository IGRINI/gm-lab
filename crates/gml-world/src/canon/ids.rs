//! Stable id derivation + a deterministic PRNG for bounded generation.
//!
//! TZ §7.3 / §12: generated ids must depend on `(parent_id, world_seed, kind)`,
//! NOT on the order of RNG draws or LLM responses, so two replays of the same
//! seed produce identical, compatible ids. We therefore derive ids by hashing
//! those inputs — and run all bounded generation choices through a small
//! deterministic PRNG seeded from the same hash. This stream is COMPLETELY
//! SEPARATE from the campaign dice RNG (`World::rng_mut`, a CPython MT19937), so
//! world generation never perturbs dice determinism (`golden_turns`).

/// FNV-1a 64-bit hash of a byte slice. Stable across platforms and runs.
fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        hash ^= b as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// Hash the structured inputs that define a generated object's identity.
pub fn hash_parts(parts: &[&str]) -> u64 {
    // Join with a NUL separator so ("ab","c") and ("a","bc") never collide.
    let mut buf: Vec<u8> = Vec::new();
    for (i, p) in parts.iter().enumerate() {
        if i > 0 {
            buf.push(0);
        }
        buf.extend_from_slice(p.as_bytes());
    }
    fnv1a64(&buf)
}

/// A stable id of the form `{kind}_{hex}` derived from world seed, parent id,
/// kind and a discriminating salt (e.g. an ordinal). Lowercase, ascii, snake —
/// compatible with `safe_id` ids used elsewhere.
pub fn stable_id(world_seed: &str, parent: &str, kind: &str, salt: &str) -> String {
    let h = hash_parts(&[world_seed, parent, kind, salt]);
    // 40 bits of hex keeps ids short but collision-safe within a campaign.
    format!("{kind}_{:010x}", h & 0xff_ffff_ffff)
}

/// A tiny splitmix64 PRNG — deterministic, seedable, no OS entropy. Used only
/// for *generation choices* (how many rooms, which template), never for dice.
#[derive(Clone, Debug)]
pub struct DetRng {
    state: u64,
}

impl DetRng {
    /// Seed from a set of string parts (same hashing as ids), so a generator's
    /// RNG stream is a pure function of its identity inputs.
    pub fn from_parts(parts: &[&str]) -> Self {
        DetRng {
            // Avoid a zero state.
            state: hash_parts(parts) | 1,
        }
    }

    /// Next raw 64-bit value (splitmix64).
    pub fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    /// Uniform integer in `[0, n)` (n > 0).
    pub fn below(&mut self, n: usize) -> usize {
        if n == 0 {
            return 0;
        }
        (self.next_u64() % n as u64) as usize
    }

    /// Inclusive range `[lo, hi]`.
    pub fn range(&mut self, lo: usize, hi: usize) -> usize {
        if hi <= lo {
            return lo;
        }
        lo + self.below(hi - lo + 1)
    }

    /// Pick a reference from a non-empty slice (panics on empty — callers guard).
    pub fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[self.below(items.len())]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stable_id_is_deterministic_and_independent_of_call_order() {
        let a = stable_id("seed1", "region_x", "place", "0");
        let b = stable_id("seed1", "region_x", "place", "0");
        assert_eq!(a, b, "same inputs -> same id");
        assert!(a.starts_with("place_"));
        // Different salt/parent/kind/seed all change the id.
        assert_ne!(a, stable_id("seed1", "region_x", "place", "1"));
        assert_ne!(a, stable_id("seed1", "region_y", "place", "0"));
        assert_ne!(a, stable_id("seed2", "region_x", "place", "0"));
        assert_ne!(a, stable_id("seed1", "region_x", "room", "0"));
    }

    #[test]
    fn det_rng_is_reproducible() {
        let mut a = DetRng::from_parts(&["seed", "x"]);
        let mut b = DetRng::from_parts(&["seed", "x"]);
        for _ in 0..32 {
            assert_eq!(a.next_u64(), b.next_u64());
        }
        // Different seeds diverge.
        let mut c = DetRng::from_parts(&["seed", "y"]);
        assert_ne!(a.next_u64(), c.next_u64());
    }
}
