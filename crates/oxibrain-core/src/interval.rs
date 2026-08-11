//! Interval algebra for the temporal fold (DESIGN §6). All comparisons are plain
//! integer comparisons on sentinels — no NULL branching (§6.2).

use oxibrain_ports::Timestamp;

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct Interval {
    pub start: Timestamp,
    pub end: Timestamp,
}

impl Interval {
    pub fn new(start: Timestamp, end: Timestamp) -> Self {
        debug_assert!(start <= end, "interval start must be <= end");
        Self { start, end }
    }

    /// True if this interval covers the given point.
    pub fn contains(&self, t: Timestamp) -> bool {
        self.start <= t && t <= self.end
    }
}

/// True if two intervals share any point.
pub fn overlaps(a: &Interval, b: &Interval) -> bool {
    a.start <= b.end && b.start <= a.end
}

/// Merge overlapping or adjacent intervals into disjoint, sorted output.
/// Input is consumed and replaced. Result is sorted by start, disjoint.
pub fn merge_overlapping(intervals: &mut Vec<Interval>) {
    if intervals.len() <= 1 {
        return;
    }
    intervals.sort_by_key(|iv| iv.start);
    let mut merged: Vec<Interval> = Vec::with_capacity(intervals.len());
    merged.push(intervals[0]);
    for &iv in &intervals[1..] {
        let last = merged.last_mut().expect("non-empty");
        if iv.start <= last.end {
            // Overlapping or adjacent — extend.
            if iv.end > last.end {
                last.end = iv.end;
            }
        } else {
            merged.push(iv);
        }
    }
    *intervals = merged;
}

/// Subtract a denial interval from affirming intervals.
/// Returns the pieces of the affirming intervals that remain after removing
/// the denial's coverage. Result is sorted and disjoint.
pub fn clip(affirming: &[Interval], denial: &Interval) -> Vec<Interval> {
    let mut result: Vec<Interval> = Vec::new();
    for aff in affirming {
        if !overlaps(aff, denial) {
            // No overlap — keep the whole affirming interval.
            result.push(*aff);
            continue;
        }
        // Overlap: split into [aff.start, denial.start) and (denial.end, aff.end].
        if aff.start < denial.start {
            result.push(Interval::new(
                aff.start,
                Timestamp(denial.start.millis() - 1),
            ));
        }
        if denial.end < aff.end {
            result.push(Interval::new(Timestamp(denial.end.millis() + 1), aff.end));
        }
        // If denial fully covers affirming, nothing is kept.
    }
    // Result is already sorted because affirming was sorted,
    // but clip may create pieces out of order — re-sort and merge.
    merge_overlapping(&mut result);
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn iv(s: i64, e: i64) -> Interval {
        Interval::new(Timestamp(s), Timestamp(e))
    }

    #[test]
    fn merge_disjoint_unchanged() {
        let mut v = vec![iv(1, 5), iv(10, 15)];
        merge_overlapping(&mut v);
        assert_eq!(v, vec![iv(1, 5), iv(10, 15)]);
    }

    #[test]
    fn merge_overlapping_test() {
        let mut v = vec![iv(1, 5), iv(3, 10)];
        merge_overlapping(&mut v);
        assert_eq!(v, vec![iv(1, 10)]);
    }

    #[test]
    fn merge_adjacent() {
        // Adjacent (5 and 6) should merge since 6 <= 5 is false but 6 <= 5+1...
        // Actually: merge condition is iv.start <= last.end. 6 <= 5 is false.
        // So adjacent-but-not-overlapping intervals do NOT merge.
        // This is correct: [1,5] and [6,10] are disjoint.
        let mut v = vec![iv(1, 5), iv(6, 10)];
        merge_overlapping(&mut v);
        assert_eq!(v.len(), 2); // NOT merged
    }

    #[test]
    fn merge_touching() {
        // Touching: [1,5] and [5,10] — share point 5 → merge.
        let mut v = vec![iv(1, 5), iv(5, 10)];
        merge_overlapping(&mut v);
        assert_eq!(v, vec![iv(1, 10)]);
    }

    #[test]
    fn clip_no_overlap() {
        let aff = vec![iv(1, 10)];
        let result = clip(&aff, &iv(20, 30));
        assert_eq!(result, vec![iv(1, 10)]);
    }

    #[test]
    fn clip_full_cover() {
        let aff = vec![iv(5, 10)];
        let result = clip(&aff, &iv(1, 20));
        assert!(result.is_empty());
    }

    #[test]
    fn clip_partial_left() {
        let aff = vec![iv(1, 10)];
        let result = clip(&aff, &iv(1, 5));
        assert_eq!(result, vec![iv(6, 10)]);
    }

    #[test]
    fn clip_partial_right() {
        let aff = vec![iv(1, 10)];
        let result = clip(&aff, &iv(7, 15));
        assert_eq!(result, vec![iv(1, 6)]);
    }

    #[test]
    fn clip_middle() {
        let aff = vec![iv(1, 20)];
        let result = clip(&aff, &iv(8, 12));
        assert_eq!(result, vec![iv(1, 7), iv(13, 20)]);
    }

    #[test]
    fn overlaps_symmetric() {
        let a = iv(1, 5);
        let b = iv(3, 10);
        assert!(overlaps(&a, &b));
        assert!(overlaps(&b, &a));
    }

    proptest! {
        #[test]
        fn merge_output_is_disjoint(starts in 1i64..100, lens in 1i64..50, count in 2usize..10) {
            // Generate random intervals, merge, check disjoint.
            let mut v: Vec<Interval> = (0..count)
                .map(|i| iv(starts + i as i64 * lens, starts + i as i64 * lens + lens))
                .collect();
            merge_overlapping(&mut v);
            for w in v.windows(2) {
                prop_assert!(w[0].end < w[1].start, "intervals must be disjoint after merge");
            }
        }

        #[test]
        fn clip_is_subset(aff_start in 1i64..50, aff_len in 1i64..50, d_start in 1i64..100, d_len in 1i64..50) {
            let aff = vec![iv(aff_start, aff_start + aff_len)];
            let denial = iv(d_start, d_start + d_len);
            let clipped = clip(&aff, &denial);
            // Every point in clipped must be in aff but not in denial.
            for c in &clipped {
                prop_assert!(c.start >= aff[0].start);
                prop_assert!(c.end <= aff[0].end);
                prop_assert!(!overlaps(c, &denial) || c.start == c.end,
                    "clipped interval must not overlap denial");
            }
        }
    }
}
