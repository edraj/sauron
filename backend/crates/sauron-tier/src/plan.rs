//! Watermark split: given a query window and the tier watermark, decide which
//! sub-range is served hot (Postgres, `occurred_at >= watermark`) and which is
//! served cold (Parquet, `occurred_at < watermark`). Half-open ranges.

use chrono::{DateTime, Utc};

/// Half-open time range `[start, end)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimeRange {
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
}

/// Which tiers a query window touches, with the exact sub-range for each.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TierPlan {
    pub hot: Option<TimeRange>,
    pub cold: Option<TimeRange>,
}

/// Split the half-open window `[from, to)` at `watermark`.
/// Everything `< watermark` is cold; everything `>= watermark` is hot. The two
/// sub-ranges are complementary → no overlap, no gap (exactly-once).
pub fn plan(watermark: DateTime<Utc>, from: DateTime<Utc>, to: DateTime<Utc>) -> TierPlan {
    if to <= from {
        return TierPlan {
            hot: None,
            cold: None,
        };
    }
    let cold = if from < watermark {
        Some(TimeRange {
            start: from,
            end: to.min(watermark),
        })
    } else {
        None
    };
    let hot = if to > watermark {
        Some(TimeRange {
            start: from.max(watermark),
            end: to,
        })
    } else {
        None
    };
    TierPlan { hot, cold }
}

/// A window split across tiers when restores are in play, so each side is a
/// SET of sub-ranges rather than at most one.
///
/// A restore puts a slice of cold data back into Postgres, and that slice can
/// sit anywhere inside the cold half — including strictly inside it, which cuts
/// the cold half in two. One `Option<TimeRange>` per tier cannot express that.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MultiTierPlan {
    pub hot: Vec<TimeRange>,
    pub cold: Vec<TimeRange>,
}

/// Merge overlapping/abutting ranges and drop empty ones, leaving a sorted,
/// disjoint set. `subtract` relies on the result being sorted and disjoint.
fn normalize(mut rs: Vec<TimeRange>) -> Vec<TimeRange> {
    rs.retain(|r| r.end > r.start);
    rs.sort_by_key(|r| r.start);
    let mut out: Vec<TimeRange> = Vec::with_capacity(rs.len());
    for r in rs {
        match out.last_mut() {
            // `<=` not `<`: abutting ranges merge, so [a,b) and [b,c) become
            // [a,c) rather than two ranges that would each become a query.
            Some(last) if r.start <= last.end => {
                if r.end > last.end {
                    last.end = r.end;
                }
            }
            _ => out.push(r),
        }
    }
    out
}

fn intersect(a: TimeRange, b: TimeRange) -> Option<TimeRange> {
    let start = a.start.max(b.start);
    let end = a.end.min(b.end);
    (end > start).then_some(TimeRange { start, end })
}

/// `base` minus every range in `cuts`.
///
/// `cuts` must be SORTED by start; overlaps between cuts are tolerated (the
/// `c.end <= cursor` guard skips a cut already consumed by an earlier one).
/// Sortedness is required because the `c.start >= base.end` early exit assumes
/// later cuts start no earlier.
fn subtract(base: TimeRange, cuts: &[TimeRange]) -> Vec<TimeRange> {
    let mut out = Vec::new();
    let mut cursor = base.start;
    for c in cuts {
        // Pure short-circuit: this cut is already consumed. The `<=` boundary is
        // deliberately NOT observable — if `c.end == cursor` then `c.start <
        // cursor` (cuts are sorted and non-empty), so falling through would push
        // nothing and leave the cursor unmoved. Writing `<` here is an
        // equivalent mutant, verified by a mutation run; don't "tighten" it
        // expecting a behaviour change, and don't add a test claiming to cover
        // it, because none can.
        if c.end <= cursor {
            continue;
        }
        if c.start >= base.end {
            break;
        }
        if c.start > cursor {
            out.push(TimeRange {
                start: cursor,
                end: c.start.min(base.end),
            });
        }
        cursor = cursor.max(c.end);
        if cursor >= base.end {
            break;
        }
    }
    if cursor < base.end {
        out.push(TimeRange {
            start: cursor,
            end: base.end,
        });
    }
    out.retain(|r| r.end > r.start);
    out
}

/// Split `[from, to)` at `watermark`, then move every restored sub-range from
/// the cold side to the hot side.
///
/// `restored` is the set of ranges a restore has put back into Postgres — one
/// entry per live `tier_pins` row for this table. Those rows exist in BOTH
/// tiers: the restore copied them out of Parquet without deleting the Parquet
/// copy, which is what makes the restore reversible. So the reader must pick
/// exactly one tier per range, and it must be Postgres — that is the copy the
/// rest of the product can actually query (indexes, joins, detail views).
///
/// Serving a restored range from both tiers would DOUBLE every count on the
/// charts; serving it from neither would blank it. The invariant the tests pin
/// down is that `hot ∪ cold` reconstructs `[from, to)` exactly, with no overlap
/// and no gap, for any set of restored ranges.
pub fn plan_with_restores(
    watermark: DateTime<Utc>,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    restored: &[TimeRange],
) -> MultiTierPlan {
    let base = plan(watermark, from, to);
    let cuts = normalize(restored.to_vec());
    let mut hot: Vec<TimeRange> = base.hot.into_iter().collect();
    let mut cold = Vec::new();
    if let Some(c) = base.cold {
        cold = subtract(c, &cuts);
        for cut in &cuts {
            if let Some(i) = intersect(c, *cut) {
                hot.push(i);
            }
        }
    }
    MultiTierPlan {
        hot: normalize(hot),
        cold: normalize(cold),
    }
}

#[cfg(test)]
mod restore_tests {
    use super::*;
    use chrono::TimeZone;

    fn t(y: i32, mo: u32, d: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(y, mo, d, 0, 0, 0).unwrap()
    }

    fn r(a: (i32, u32, u32), b: (i32, u32, u32)) -> TimeRange {
        TimeRange {
            start: t(a.0, a.1, a.2),
            end: t(b.0, b.1, b.2),
        }
    }

    /// hot ∪ cold must reconstruct [from, to) exactly: sorted, merged, one range.
    fn assert_covers(p: &MultiTierPlan, from: DateTime<Utc>, to: DateTime<Utc>) {
        let mut all = p.hot.clone();
        all.extend(p.cold.clone());
        // Overlap check BEFORE merging — normalize would silently absorb an
        // overlap and this assertion would then prove nothing.
        let mut sorted = all.clone();
        sorted.sort_by_key(|x| x.start);
        for w in sorted.windows(2) {
            assert!(
                w[0].end <= w[1].start,
                "hot and cold overlap: {:?} and {:?}",
                w[0],
                w[1]
            );
        }
        let merged = normalize(all);
        assert_eq!(
            merged,
            vec![TimeRange {
                start: from,
                end: to
            }],
            "coverage gap"
        );
    }

    // -- The two helpers, tested directly ---------------------------------
    //
    // Both of these exist because a mutation run showed that going through
    // `plan_with_restores` alone could not falsify them: `subtract` has its own
    // empty-range filter and `intersect` is strict, so normalize's filter never
    // changed an observable result, and `subtract`'s overlap guard is
    // unreachable when its input has already been merged. Untestable through the
    // front door is not the same as unnecessary — these pin the helpers' own
    // contracts, which is what the guards are actually for.

    #[test]
    fn normalize_drops_empty_ranges() {
        let out = normalize(vec![
            r((2026, 5, 5), (2026, 5, 5)),
            r((2026, 5, 10), (2026, 5, 12)),
        ]);
        assert_eq!(out, vec![r((2026, 5, 10), (2026, 5, 12))]);
    }

    #[test]
    fn normalize_merges_overlapping_and_abutting() {
        let out = normalize(vec![
            r((2026, 5, 10), (2026, 5, 12)),
            r((2026, 5, 1), (2026, 5, 5)),
            r((2026, 5, 5), (2026, 5, 8)),   // abuts the previous
            r((2026, 5, 11), (2026, 5, 20)), // overlaps the first
        ]);
        assert_eq!(
            out,
            vec![
                r((2026, 5, 1), (2026, 5, 8)),
                r((2026, 5, 10), (2026, 5, 20))
            ]
        );
    }

    #[test]
    fn subtract_tolerates_sorted_but_overlapping_cuts() {
        // The second cut is entirely swallowed by the first. Without the
        // `c.end <= cursor` guard the cursor would rewind and emit a bogus
        // duplicate slice.
        let out = subtract(
            r((2026, 5, 1), (2026, 6, 1)),
            &[
                r((2026, 5, 5), (2026, 5, 20)),
                r((2026, 5, 8), (2026, 5, 12)),
            ],
        );
        assert_eq!(
            out,
            vec![
                r((2026, 5, 1), (2026, 5, 5)),
                r((2026, 5, 20), (2026, 6, 1))
            ]
        );
    }

    #[test]
    fn subtract_with_a_cut_ending_exactly_at_the_start() {
        let out = subtract(
            r((2026, 5, 10), (2026, 6, 1)),
            &[r((2026, 5, 1), (2026, 5, 10))],
        );
        assert_eq!(out, vec![r((2026, 5, 10), (2026, 6, 1))]);
    }

    #[test]
    fn no_restores_matches_the_plain_split() {
        let p = plan_with_restores(t(2026, 6, 1), t(2026, 5, 15), t(2026, 6, 15), &[]);
        assert_eq!(p.cold, vec![r((2026, 5, 15), (2026, 6, 1))]);
        assert_eq!(p.hot, vec![r((2026, 6, 1), (2026, 6, 15))]);
        assert_covers(&p, t(2026, 5, 15), t(2026, 6, 15));
    }

    /// The case the whole feature exists for: a restore strictly inside the cold
    /// half splits cold in two and adds a hot island between them.
    #[test]
    fn interior_restore_splits_cold_in_two() {
        let p = plan_with_restores(
            t(2026, 6, 1),
            t(2026, 5, 1),
            t(2026, 6, 1),
            &[r((2026, 5, 10), (2026, 5, 12))],
        );
        assert_eq!(
            p.cold,
            vec![
                r((2026, 5, 1), (2026, 5, 10)),
                r((2026, 5, 12), (2026, 6, 1))
            ]
        );
        assert_eq!(p.hot, vec![r((2026, 5, 10), (2026, 5, 12))]);
        assert_covers(&p, t(2026, 5, 1), t(2026, 6, 1));
    }

    #[test]
    fn a_restored_range_never_appears_on_the_cold_side() {
        let restored = r((2026, 5, 10), (2026, 5, 12));
        let p = plan_with_restores(t(2026, 6, 1), t(2026, 5, 1), t(2026, 6, 1), &[restored]);
        for c in &p.cold {
            assert!(
                intersect(*c, restored).is_none(),
                "cold range {c:?} overlaps a restored range — rows would be counted twice"
            );
        }
    }

    #[test]
    fn restore_abutting_the_watermark_merges_into_the_hot_half() {
        let p = plan_with_restores(
            t(2026, 6, 1),
            t(2026, 5, 1),
            t(2026, 6, 15),
            &[r((2026, 5, 20), (2026, 6, 1))],
        );
        // The restored slice and the natural hot half are contiguous, so they
        // become ONE hot range rather than two adjacent queries.
        assert_eq!(p.hot, vec![r((2026, 5, 20), (2026, 6, 15))]);
        assert_eq!(p.cold, vec![r((2026, 5, 1), (2026, 5, 20))]);
        assert_covers(&p, t(2026, 5, 1), t(2026, 6, 15));
    }

    #[test]
    fn restore_covering_the_whole_cold_half_leaves_cold_empty() {
        let p = plan_with_restores(
            t(2026, 6, 1),
            t(2026, 5, 1),
            t(2026, 6, 1),
            &[r((2026, 4, 1), (2026, 7, 1))],
        );
        assert!(p.cold.is_empty());
        assert_eq!(p.hot, vec![r((2026, 5, 1), (2026, 6, 1))]);
        assert_covers(&p, t(2026, 5, 1), t(2026, 6, 1));
    }

    #[test]
    fn restores_entirely_outside_the_window_change_nothing() {
        let p = plan_with_restores(
            t(2026, 6, 1),
            t(2026, 5, 10),
            t(2026, 5, 20),
            &[r((2026, 1, 1), (2026, 2, 1)), r((2026, 8, 1), (2026, 9, 1))],
        );
        assert_eq!(p.cold, vec![r((2026, 5, 10), (2026, 5, 20))]);
        assert!(p.hot.is_empty());
        assert_covers(&p, t(2026, 5, 10), t(2026, 5, 20));
    }

    /// Pins are allowed to overlap — each restore creates its own, and two
    /// restores of intersecting ranges are legitimate. If overlap were not
    /// collapsed, the same slice would be queried twice on the hot side.
    #[test]
    fn overlapping_restores_are_collapsed_not_double_counted() {
        let p = plan_with_restores(
            t(2026, 6, 1),
            t(2026, 5, 1),
            t(2026, 6, 1),
            &[
                r((2026, 5, 5), (2026, 5, 15)),
                r((2026, 5, 10), (2026, 5, 20)),
            ],
        );
        assert_eq!(p.hot, vec![r((2026, 5, 5), (2026, 5, 20))]);
        assert_eq!(
            p.cold,
            vec![
                r((2026, 5, 1), (2026, 5, 5)),
                r((2026, 5, 20), (2026, 6, 1))
            ]
        );
        assert_covers(&p, t(2026, 5, 1), t(2026, 6, 1));
    }

    #[test]
    fn unsorted_and_empty_restores_are_tolerated() {
        let p = plan_with_restores(
            t(2026, 6, 1),
            t(2026, 5, 1),
            t(2026, 6, 1),
            &[
                r((2026, 5, 20), (2026, 5, 22)),
                r((2026, 5, 5), (2026, 5, 5)), // empty, must be ignored
                r((2026, 5, 10), (2026, 5, 12)),
            ],
        );
        assert_eq!(
            p.hot,
            vec![
                r((2026, 5, 10), (2026, 5, 12)),
                r((2026, 5, 20), (2026, 5, 22))
            ]
        );
        assert_covers(&p, t(2026, 5, 1), t(2026, 6, 1));
    }

    #[test]
    fn an_all_hot_window_is_untouched_by_restores() {
        // Restores only ever move ranges OUT of the cold half. A window entirely
        // above the watermark has no cold half to cut.
        let p = plan_with_restores(
            t(2026, 6, 1),
            t(2026, 6, 10),
            t(2026, 6, 20),
            &[r((2026, 6, 12), (2026, 6, 14))],
        );
        assert_eq!(p.hot, vec![r((2026, 6, 10), (2026, 6, 20))]);
        assert!(p.cold.is_empty());
        assert_covers(&p, t(2026, 6, 10), t(2026, 6, 20));
    }

    #[test]
    fn empty_window_yields_nothing_even_with_restores() {
        let p = plan_with_restores(
            t(2026, 6, 1),
            t(2026, 6, 5),
            t(2026, 6, 5),
            &[r((2026, 1, 1), (2027, 1, 1))],
        );
        assert!(p.hot.is_empty() && p.cold.is_empty());
    }

    #[test]
    fn many_interleaved_restores_still_tile_the_window_exactly() {
        let p = plan_with_restores(
            t(2026, 6, 1),
            t(2026, 5, 1),
            t(2026, 6, 20),
            &[
                r((2026, 5, 2), (2026, 5, 3)),
                r((2026, 5, 7), (2026, 5, 9)),
                r((2026, 5, 15), (2026, 5, 16)),
                r((2026, 5, 28), (2026, 5, 30)),
            ],
        );
        assert_covers(&p, t(2026, 5, 1), t(2026, 6, 20));
        assert_eq!(p.hot.len(), 5); // four islands + the natural hot half
        assert_eq!(p.cold.len(), 5);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn t(y: i32, mo: u32, d: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(y, mo, d, 0, 0, 0).unwrap()
    }

    #[test]
    fn fully_hot_when_window_after_watermark() {
        let p = plan(t(2026, 6, 1), t(2026, 6, 10), t(2026, 6, 20));
        assert_eq!(
            p.hot,
            Some(TimeRange {
                start: t(2026, 6, 10),
                end: t(2026, 6, 20)
            })
        );
        assert_eq!(p.cold, None);
    }

    #[test]
    fn fully_cold_when_window_before_watermark() {
        let p = plan(t(2026, 6, 1), t(2026, 5, 1), t(2026, 5, 20));
        assert_eq!(
            p.cold,
            Some(TimeRange {
                start: t(2026, 5, 1),
                end: t(2026, 5, 20)
            })
        );
        assert_eq!(p.hot, None);
    }

    #[test]
    fn straddle_splits_at_watermark_with_no_overlap() {
        let p = plan(t(2026, 6, 1), t(2026, 5, 15), t(2026, 6, 15));
        assert_eq!(
            p.cold,
            Some(TimeRange {
                start: t(2026, 5, 15),
                end: t(2026, 6, 1)
            })
        );
        assert_eq!(
            p.hot,
            Some(TimeRange {
                start: t(2026, 6, 1),
                end: t(2026, 6, 15)
            })
        );
    }

    #[test]
    fn boundary_exactly_at_watermark_is_hot_side_empty() {
        // window [from, watermark): entirely cold, hot omitted (to == watermark).
        let p = plan(t(2026, 6, 1), t(2026, 5, 1), t(2026, 6, 1));
        assert_eq!(
            p.cold,
            Some(TimeRange {
                start: t(2026, 5, 1),
                end: t(2026, 6, 1)
            })
        );
        assert_eq!(p.hot, None);
    }

    #[test]
    fn empty_or_inverted_window_yields_nothing() {
        assert_eq!(
            plan(t(2026, 6, 1), t(2026, 6, 5), t(2026, 6, 5)),
            TierPlan {
                hot: None,
                cold: None
            }
        );
        assert_eq!(
            plan(t(2026, 6, 1), t(2026, 6, 9), t(2026, 6, 5)),
            TierPlan {
                hot: None,
                cold: None
            }
        );
    }
}
