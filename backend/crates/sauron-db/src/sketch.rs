//! Mergeable sketches backing the dashboard rollups: a HyperLogLog for
//! distinct-user counts and a log-scale histogram for latency percentiles.
//!
//! Both exist because their exact counterparts cannot be pre-aggregated:
//! `count(DISTINCT …)` and `percentile_cont` over an arbitrary window need the
//! raw rows, which is precisely what the rollup architecture forbids the
//! request path from touching. A sketch per (day, key) merges across days,
//! environments and tiers in microseconds, at a disclosed accuracy cost
//! (`docs/approximate-analytics.md`).
//!
//! # Why the hash is SHA-256
//!
//! HLL needs a hash that is (a) uniform and (b) STABLE across processes,
//! builds and years — a sketch written today is merged with one written next
//! month by a different binary. `std`'s hashers are randomly keyed per
//! process, so they are disqualified outright. `sha2` is already a workspace
//! dependency, its output is definitionally stable, and at fold volumes
//! (thousands of rows per cycle) its cost is noise. Do NOT swap it for a
//! faster non-cryptographic hash without versioning the sketch bytes.
//!
//! # Precision
//!
//! p=12 → m=4096 registers → one byte each → a 4 KiB dense sketch with a
//! standard error of 1.04/√4096 ≈ 1.6%. Dense always: a sparse encoding would
//! save space on small sets but adds a format fork that every future reader
//! must handle; 4 KiB per rollup row is affordable at this cardinality.

use sha2::{Digest, Sha256};

const P: u32 = 12;
const M: usize = 1 << P; // 4096

/// Dense HyperLogLog, p=12. `Default`-able, byte-serializable, mergeable.
#[derive(Clone, PartialEq)]
pub struct Hll {
    registers: Vec<u8>,
}

impl Default for Hll {
    fn default() -> Self {
        Self::new()
    }
}

impl Hll {
    pub fn new() -> Self {
        Self {
            registers: vec![0u8; M],
        }
    }

    /// Whether nothing was ever inserted — callers use this to store SQL NULL
    /// instead of 4 KiB of zeroes for empty buckets.
    pub fn is_empty(&self) -> bool {
        self.registers.iter().all(|&r| r == 0)
    }

    pub fn insert(&mut self, item: &str) {
        let digest = Sha256::digest(item.as_bytes());
        let h = u64::from_be_bytes(digest[..8].try_into().expect("8 bytes"));
        let idx = (h >> (64 - P)) as usize;
        // Rank of the first set bit in the remaining 52 bits; an all-zero
        // remainder caps at 53 rather than overflowing the register.
        let w = h << P;
        let rho = (w.leading_zeros() + 1).min(64 - P + 1) as u8;
        if self.registers[idx] < rho {
            self.registers[idx] = rho;
        }
    }

    pub fn merge(&mut self, other: &Hll) {
        for (a, b) in self.registers.iter_mut().zip(other.registers.iter()) {
            if *a < *b {
                *a = *b;
            }
        }
    }

    /// Bias-corrected estimate with linear counting for the small range —
    /// the standard HLL estimator, minus the large-range correction that a
    /// 64-bit hash makes irrelevant at any cardinality this system can see.
    pub fn estimate(&self) -> i64 {
        let m = M as f64;
        let alpha = 0.7213 / (1.0 + 1.079 / m);
        let mut sum = 0.0f64;
        let mut zeros = 0usize;
        for &r in &self.registers {
            sum += 2f64.powi(-i32::from(r));
            if r == 0 {
                zeros += 1;
            }
        }
        let raw = alpha * m * m / sum;
        let est = if raw <= 2.5 * m && zeros > 0 {
            m * (m / zeros as f64).ln()
        } else {
            raw
        };
        est.round() as i64
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        self.registers.clone()
    }

    /// `None` on any length but exactly 4096 — a truncated or foreign blob
    /// must fail loudly at the read site, not estimate garbage.
    pub fn from_bytes(b: &[u8]) -> Option<Hll> {
        (b.len() == M).then(|| Hll {
            registers: b.to_vec(),
        })
    }

    /// Convenience for reading nullable bytea columns.
    pub fn from_opt(b: Option<&Vec<u8>>) -> Hll {
        b.and_then(|v| Hll::from_bytes(v)).unwrap_or_default()
    }
}

impl std::fmt::Debug for Hll {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Hll(est={})", self.estimate())
    }
}

/// √2-ladder latency histogram: bucket 0 is `[0, 1) ms`, bucket i≥1 covers
/// `[√2^(i-1), √2^i) ms`, bucket 55 is open above (≈ 37 h). Value error of a
/// derived percentile is bounded by one bucket ratio (√2) at distribution
/// edges and far tighter in the interior thanks to geometric interpolation.
pub const HIST_BUCKETS: usize = 56;

#[derive(Clone, Debug, PartialEq)]
pub struct LatencyHistogram {
    counts: [i64; HIST_BUCKETS],
}

impl Default for LatencyHistogram {
    fn default() -> Self {
        Self::new()
    }
}

fn bucket_lower(i: usize) -> f64 {
    if i == 0 {
        0.0
    } else {
        2f64.powf((i as f64 - 1.0) / 2.0)
    }
}

impl LatencyHistogram {
    pub fn new() -> Self {
        Self {
            counts: [0; HIST_BUCKETS],
        }
    }

    pub fn record(&mut self, ms: f64) {
        if ms.is_nan() {
            return;
        }
        let idx = if ms < 1.0 {
            0
        } else {
            ((ms.log2() * 2.0).floor() as usize + 1).min(HIST_BUCKETS - 1)
        };
        self.counts[idx] += 1;
    }

    pub fn merge_counts(&mut self, other: &[i64]) {
        for (i, v) in other.iter().enumerate().take(HIST_BUCKETS) {
            self.counts[i] += v;
        }
    }

    pub fn counts(&self) -> Vec<i64> {
        self.counts.to_vec()
    }

    pub fn from_counts(c: &[i64]) -> Self {
        let mut h = Self::new();
        h.merge_counts(c);
        h
    }

    pub fn total(&self) -> i64 {
        self.counts.iter().sum()
    }

    /// 448-byte little-endian `i64×56` wire form for the `bytea` rollup
    /// columns — `bigint[]` cannot ride the house `unnest($n::type[])` bulk
    /// upserts (an array of arrays flattens), bytes can.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(HIST_BUCKETS * 8);
        for c in &self.counts {
            out.extend_from_slice(&c.to_le_bytes());
        }
        out
    }

    /// Tolerant decode: shorter blobs read as zero-padded, longer are
    /// truncated — a schema that grows buckets later stays readable.
    // clippy suggests `as_chunks`, which is Rust 1.88+; workspace MSRV is 1.82.
    // The lint itself is clippy 1.98+, so older toolchains need unknown_lints.
    #[allow(unknown_lints)]
    #[allow(clippy::chunks_exact_to_as_chunks)]
    pub fn counts_from_bytes(b: &[u8]) -> Vec<i64> {
        let mut counts = vec![0i64; HIST_BUCKETS];
        for (i, chunk) in b.chunks_exact(8).enumerate().take(HIST_BUCKETS) {
            counts[i] = i64::from_le_bytes(chunk.try_into().expect("8 bytes"));
        }
        counts
    }

    pub fn from_bytes(b: &[u8]) -> Self {
        Self::from_counts(&Self::counts_from_bytes(b))
    }

    /// Percentile via cumulative walk + geometric interpolation inside the
    /// landing bucket. Returns 0.0 for an empty histogram — the same shape
    /// `percentile_cont` over zero rows would surface as NULL→0 downstream.
    pub fn percentile(&self, q: f64) -> f64 {
        let total = self.total();
        if total == 0 {
            return 0.0;
        }
        let q = q.clamp(0.0, 1.0);
        let target = q * total as f64;
        let mut cum = 0i64;
        for i in 0..HIST_BUCKETS {
            let c = self.counts[i];
            if c == 0 {
                continue;
            }
            if (cum + c) as f64 >= target {
                let f = ((target - cum as f64) / c as f64).clamp(0.0, 1.0);
                if i == 0 {
                    return f; // [0,1) ms: linear is exact enough
                }
                let lo = bucket_lower(i);
                // Top bucket has no upper bound; treat it as one ratio wide.
                return lo * 2f64.powf(f * 0.5);
            }
            cum += c;
        }
        bucket_lower(HIST_BUCKETS - 1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hll_estimates_50k_within_3pct() {
        let mut h = Hll::new();
        for i in 0..50_000 {
            h.insert(&format!("user_{i}"));
        }
        let est = h.estimate();
        assert!((est - 50_000).abs() < 1_500, "est={est}");
    }

    #[test]
    fn hll_small_range_is_near_exact() {
        let mut h = Hll::new();
        for i in 0..100 {
            h.insert(&format!("u{i}"));
        }
        let est = h.estimate();
        assert!((est - 100).abs() <= 5, "est={est}");
        assert!(!h.is_empty());
        assert!(Hll::new().is_empty());
    }

    #[test]
    fn hll_merge_equals_union() {
        let mut a = Hll::new();
        let mut b = Hll::new();
        for i in 0..30_000 {
            a.insert(&format!("user_{i}"));
        }
        for i in 20_000..50_000 {
            b.insert(&format!("user_{i}"));
        }
        let ea = a.estimate();
        let eb = b.estimate();
        a.merge(&b);
        let union = a.estimate();
        assert!((union - 50_000).abs() < 1_500, "union={union}");
        assert!(union >= ea.max(eb));
    }

    #[test]
    fn hll_roundtrip_bytes() {
        let mut h = Hll::new();
        for i in 0..1_000 {
            h.insert(&format!("x{i}"));
        }
        let bytes = h.to_bytes();
        assert_eq!(bytes.len(), 4096);
        let back = Hll::from_bytes(&bytes).expect("valid length");
        assert_eq!(back.estimate(), h.estimate());
        assert!(Hll::from_bytes(&bytes[..100]).is_none());
    }

    #[test]
    fn hll_insert_is_idempotent() {
        let mut h = Hll::new();
        for _ in 0..10 {
            h.insert("same-user");
        }
        assert_eq!(h.estimate(), 1);
    }

    #[test]
    fn hist_p50_interpolates_tightly() {
        let mut h = LatencyHistogram::new();
        for v in 1..=10_000 {
            h.record(v as f64);
        }
        let p50 = h.percentile(0.5);
        assert!((4_700.0..=5_300.0).contains(&p50), "p50={p50}");
    }

    #[test]
    fn hist_p99_lands_in_true_bucket() {
        let mut h = LatencyHistogram::new();
        for v in 1..=10_000 {
            h.record(v as f64);
        }
        // True p99 = 9 900, whose bucket is [8192, 11585). Tail percentiles
        // are bucket-accurate, not point-accurate — the disclosed contract.
        let p99 = h.percentile(0.99);
        assert!((8_192.0..11_586.0).contains(&p99), "p99={p99}");
    }

    #[test]
    fn hist_merge_adds_and_roundtrips() {
        let mut a = LatencyHistogram::new();
        let mut b = LatencyHistogram::new();
        for v in [5.0, 50.0, 500.0] {
            a.record(v);
        }
        for v in [5.0, 5_000.0] {
            b.record(v);
        }
        a.merge_counts(&b.counts());
        assert_eq!(a.total(), 5);
        let back = LatencyHistogram::from_counts(&a.counts());
        assert_eq!(back, a);
    }

    #[test]
    fn hist_bytes_roundtrip() {
        let mut h = LatencyHistogram::new();
        for v in [1.0, 42.0, 9_000.0, 1e9] {
            h.record(v);
        }
        let b = h.to_bytes();
        assert_eq!(b.len(), HIST_BUCKETS * 8);
        assert_eq!(LatencyHistogram::from_bytes(&b), h);
        assert_eq!(
            LatencyHistogram::counts_from_bytes(&[]),
            vec![0i64; HIST_BUCKETS]
        );
    }

    #[test]
    fn hist_edges_do_not_panic() {
        let mut h = LatencyHistogram::new();
        h.record(f64::NAN);
        h.record(-5.0);
        h.record(0.0);
        h.record(1e12);
        assert_eq!(h.total(), 3);
        assert_eq!(LatencyHistogram::new().percentile(0.5), 0.0);
    }
}
