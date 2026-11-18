//! Regions such as `c1:101-150`, as typed and as written in the log.

use crate::bam::{Contig, grouped};
use crate::{Error, Result};

/// A stretch of one contig, 1-based and inclusive at both ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Region {
    /// The contig's place in the BAM's header, from 0.
    pub contig: usize,
    pub start: u64,
    pub end: u64,
}

impl Region {
    /// The region as written in file names and FASTA headers: `c1:101-110`.
    pub fn label(&self, contigs: &[Contig]) -> String {
        format!("{}:{}-{}", contigs[self.contig].name, self.start, self.end)
    }
}

/// Read a region typed as `ctg`, `ctg:pos` or `ctg:start-end` (commas
/// allowed in numbers). A contig name that itself holds `:` is matched whole
/// first; `{name}:start-end` makes it explicit. An end past the contig's end
/// is cut to it, and the note says so. `file` names the BAM in messages.
pub fn parse_region(
    text: &str,
    contigs: &[Contig],
    file: &str,
) -> Result<(Region, Option<String>)> {
    let invalid = |why: &str| Error::Invalid(format!("'{text}' is not a region: {why}"));
    let find = |name: &str| contigs.iter().position(|c| c.name == name);
    let whole = |contig: usize| Region {
        contig,
        start: 1,
        end: contigs[contig].length,
    };
    if let Some(contig) = find(text) {
        return Ok((whole(contig), None));
    }
    let (name, range) = if let Some(rest) = text.strip_prefix('{') {
        let (name, after) = rest
            .split_once('}')
            .ok_or_else(|| invalid("a { needs its }"))?;
        match after {
            "" => (name, None),
            _ => (
                name,
                Some(
                    after
                        .strip_prefix(':')
                        .ok_or_else(|| invalid("write it as {contig}:start-end"))?,
                ),
            ),
        }
    } else {
        match text.rsplit_once(':') {
            Some((name, range)) => (name, Some(range)),
            None => (text, None),
        }
    };
    let contig =
        find(name).ok_or_else(|| Error::Invalid(format!("no contig named '{name}' in {file}")))?;
    let Some(range) = range else {
        return Ok((whole(contig), None));
    };
    let number = |s: &str| -> Result<u64> {
        s.replace(',', "")
            .parse::<u64>()
            .map_err(|_| invalid("write it as contig:start-end"))
    };
    let (start, end) = match range.split_once('-') {
        Some((a, b)) => (number(a)?, number(b)?),
        None => {
            let pos = number(range)?;
            (pos, pos)
        }
    };
    if start == 0 || end == 0 {
        return Err(invalid("positions count from 1"));
    }
    if end < start {
        return Err(invalid("the end is before the start"));
    }
    let length = contigs[contig].length;
    if start > length {
        return Err(Error::Invalid(format!(
            "{name} is {} bp; {} is past its end",
            grouped(length),
            grouped(start)
        )));
    }
    let region = Region {
        contig,
        start,
        end: end.min(length),
    };
    let note = (end > length).then(|| {
        format!(
            "{name} ends at {}, so the region was cut to {}",
            grouped(length),
            region.label(contigs)
        )
    });
    Ok((region, note))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bam::Contig;

    fn test_contigs() -> Vec<Contig> {
        [
            ("c1", 1000),
            ("c2", 500),
            ("HLA-A*01:01", 300),
            ("x:1-5", 50),
        ]
        .into_iter()
        .map(|(name, length)| Contig {
            name: name.to_string(),
            length,
            mapped: None,
            unmapped: None,
        })
        .collect()
    }

    #[test]
    fn regions_parse_whole_contigs_points_and_ranges() {
        let contigs = test_contigs();
        let p = |t: &str| parse_region(t, &contigs, "reads.bam");
        let err = |t: &str| p(t).unwrap_err().to_string();
        let r = |contig, start, end| Region { contig, start, end };
        assert_eq!(p("c1").unwrap().0, r(0, 1, 1000));
        assert_eq!(p("c1:150").unwrap().0, r(0, 150, 150));
        assert_eq!(
            p("c1:1,001-1,000").unwrap_err().to_string(),
            "'c1:1,001-1,000' is not a region: the end is before the start"
        );
        // Commas are dropped from numbers, so 12,4 is 124.
        assert_eq!(
            err("c2:12,4-49"),
            "'c2:12,4-49' is not a region: the end is before the start"
        );
        assert_eq!(p("HLA-A*01:01").unwrap().0.contig, 2);
        assert_eq!(p("HLA-A*01:01:5-9").unwrap().0, r(2, 5, 9));
        // A whole contig name wins over reading it as a region.
        assert_eq!(p("x:1-5").unwrap().0, r(3, 1, 50));
        assert_eq!(p("{x:1-5}:2-3").unwrap().0, r(3, 2, 3));
        let (cut, note) = p("c1:901-5000").unwrap();
        assert_eq!((cut.start, cut.end), (901, 1000));
        assert_eq!(
            note.unwrap(),
            "c1 ends at 1,000, so the region was cut to c1:901-1000"
        );
        assert_eq!(err("c1:1200-1300"), "c1 is 1,000 bp; 1,200 is past its end");
        assert_eq!(err("c9:1-5"), "no contig named 'c9' in reads.bam");
        assert_eq!(
            err("c1:0-5"),
            "'c1:0-5' is not a region: positions count from 1"
        );
        assert_eq!(
            err("c1:x-5"),
            "'c1:x-5' is not a region: write it as contig:start-end"
        );
        assert_eq!(cut.label(&contigs), "c1:901-1000");
        assert_eq!(crate::bam::grouped(1_204_551), "1,204,551");
        assert_eq!(crate::bam::grouped(7), "7");
    }
}
