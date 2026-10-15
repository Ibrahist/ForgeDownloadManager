/// Half-open byte ranges `[start, end)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ByteRange {
    pub start: u64,
    pub end: u64,
}

impl ByteRange {
    pub fn len(self) -> u64 {
        self.end.saturating_sub(self.start)
    }
}

/// Split `total` bytes into at most `requested` segments (clamped to 1–32).
///
/// `requested` is a maximum. A segment is never shorter than `min_segment_bytes`
/// unless the whole file is smaller than that, in which case there is one segment.
/// The pieces cover `[0, total)` exactly and do not overlap.
pub fn plan_segments(total: u64, requested: u32, min_segment_bytes: u64) -> Vec<ByteRange> {
    if total == 0 {
        return Vec::new();
    }
    let requested = u64::from(requested.clamp(1, 32));
    let min_segment_bytes = min_segment_bytes.max(1);
    let max_by_size = (total / min_segment_bytes).max(1);
    let n = requested.min(max_by_size);
    let base = total / n;
    let extra = total % n;
    let mut out = Vec::with_capacity(n as usize);
    let mut start = 0u64;
    for i in 0..n {
        let len = base + if i < extra { 1 } else { 0 };
        let end = start + len;
        out.push(ByteRange { start, end });
        start = end;
    }
    debug_assert_eq!(start, total);
    out
}

pub fn validate_coverage(total: u64, parts: &[ByteRange]) -> Result<(), String> {
    if total == 0 {
        return if parts.is_empty() {
            Ok(())
        } else {
            Err("empty file produced segments".into())
        };
    }
    if parts.is_empty() {
        return Err("no segments".into());
    }
    let mut cursor = 0u64;
    for (i, part) in parts.iter().enumerate() {
        if part.start != cursor {
            return Err(format!("segment {i} starts at {} but coverage is at {cursor}", part.start));
        }
        if part.end <= part.start {
            return Err(format!("segment {i} is empty or inverted"));
        }
        cursor = part.end;
    }
    if cursor != total {
        return Err(format!("coverage ends at {cursor}, file is {total}"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn empty_file_has_no_segments() {
        assert!(plan_segments(0, 8, 1024).is_empty());
    }

    #[test]
    fn small_file_is_one_segment() {
        let parts = plan_segments(10, 8, 1024 * 1024);
        assert_eq!(parts, vec![ByteRange { start: 0, end: 10 }]);
    }

    #[test]
    fn remainder_is_spread_across_the_first_segments() {
        let parts = plan_segments(10, 3, 1);
        assert_eq!(
            parts,
            vec![
                ByteRange { start: 0, end: 4 },
                ByteRange { start: 4, end: 7 },
                ByteRange { start: 7, end: 10 },
            ]
        );
    }

    #[test]
    fn requested_count_above_32_is_clamped() {
        let parts = plan_segments(10_000, 100, 1);
        assert_eq!(parts.len(), 32);
        validate_coverage(10_000, &parts).unwrap();
    }

    #[test]
    fn minimum_size_reduces_the_count() {
        let parts = plan_segments(2_500_000, 8, 1_000_000);
        assert_eq!(parts.len(), 2);
        assert!(parts.iter().all(|p| p.len() >= 1_000_000));
        validate_coverage(2_500_000, &parts).unwrap();
    }

    proptest! {
        #[test]
        fn segments_cover_the_file_without_overlap(
            total in 0u64..5_000_000,
            requested in 0u32..64,
            min_segment in 1u64..500_000,
        ) {
            let parts = plan_segments(total, requested, min_segment);
            validate_coverage(total, &parts).unwrap();
            let cap = u64::from(requested.clamp(1, 32));
            prop_assert!(parts.len() as u64 <= cap || total == 0);
            if total > 0 {
                prop_assert!(!parts.is_empty());
                if total >= min_segment * 2 && requested >= 2 {
                    prop_assert!(parts.iter().all(|p| p.len() >= min_segment.min(total)));
                }
            }
        }
    }
}
