//! Lists of positions as written in commands, such as `3-7,12`.
//!
//! Text is 1-based and inclusive, because that is how people count rows and
//! columns. Inside, ranges are 0-based and half-open.

use std::fmt;
use std::ops::Range;
use std::str::FromStr;

/// A set of positions: sorted, merged and never empty.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ranges(Vec<Range<usize>>);

impl Ranges {
    /// Sort and merge `ranges` (0-based, half-open). `None` if no position is left.
    pub fn new(ranges: impl IntoIterator<Item = Range<usize>>) -> Option<Ranges> {
        let mut sorted: Vec<Range<usize>> =
            ranges.into_iter().filter(|r| r.start < r.end).collect();
        sorted.sort_by_key(|r| r.start);
        let mut merged: Vec<Range<usize>> = Vec::with_capacity(sorted.len());
        for range in sorted {
            match merged.last_mut() {
                Some(last) if range.start <= last.end => last.end = last.end.max(range.end),
                _ => merged.push(range),
            }
        }
        (!merged.is_empty()).then_some(Ranges(merged))
    }

    /// The ranges, 0-based and half-open, in ascending order.
    pub fn as_slice(&self) -> &[Range<usize>] {
        &self.0
    }

    /// How many positions are covered.
    pub fn count(&self) -> usize {
        self.0.iter().map(|r| r.end - r.start).sum()
    }

    /// One past the highest position.
    pub fn end(&self) -> usize {
        self.0.last().map_or(0, |r| r.end)
    }
}

impl FromStr for Ranges {
    type Err = String;

    fn from_str(text: &str) -> Result<Ranges, String> {
        if text.trim().is_empty() {
            return Err("empty range list".to_string());
        }
        let mut ranges = Vec::new();
        for part in text.split(',') {
            let part = part.trim();
            let bad = || format!("bad range '{part}': use forms like 5 or 10-20");
            let (first, last) = match part.split_once('-') {
                Some((a, b)) => (a.trim(), b.trim()),
                None => (part, part),
            };
            let first: usize = first.parse().map_err(|_| bad())?;
            let last: usize = last.parse().map_err(|_| bad())?;
            if first == 0 || last == 0 {
                return Err("positions start at 1".to_string());
            }
            if first > last {
                return Err(format!("range {first}-{last} runs backwards"));
            }
            ranges.push(first - 1..last);
        }
        Ranges::new(ranges).ok_or_else(|| "empty range list".to_string())
    }
}

impl fmt::Display for Ranges {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (index, range) in self.0.iter().enumerate() {
            if index > 0 {
                f.write_str(",")?;
            }
            if range.end - range.start == 1 {
                write!(f, "{}", range.start + 1)?;
            } else {
                write!(f, "{}-{}", range.start + 1, range.end)?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_prints() {
        let r: Ranges = "3-7,12".parse().unwrap();
        assert_eq!(r.as_slice(), [2..7, 11..12]);
        assert_eq!(r.to_string(), "3-7,12");
        assert_eq!((r.count(), r.end()), (6, 12));
    }

    #[test]
    fn sorts_and_merges_overlapping_and_touching() {
        assert_eq!("5,1-3,2-4".parse::<Ranges>().unwrap().to_string(), "1-5");
    }

    #[test]
    fn rejects_bad_text() {
        for (text, message) in [
            ("", "empty range list"),
            ("0", "positions start at 1"),
            ("7-3", "range 7-3 runs backwards"),
            ("a-b", "bad range 'a-b': use forms like 5 or 10-20"),
            ("1-", "bad range '1-': use forms like 5 or 10-20"),
            ("1,,2", "bad range '': use forms like 5 or 10-20"),
        ] {
            assert_eq!(text.parse::<Ranges>().unwrap_err(), message, "{text:?}");
        }
    }

    #[test]
    fn new_normalises() {
        assert_eq!(
            Ranges::new([4..6, 0..2, 1..3]).unwrap().as_slice(),
            [0..3, 4..6]
        );
        assert_eq!(Ranges::new(std::iter::once(3..3)), None);
    }
}
