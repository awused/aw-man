use std::cmp::Ordering;

use once_cell::sync::Lazy;
use ouroboros::self_referencing;
use regex::Regex;
use Segment::*;

static SEGMENT_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"([^\d.]*)((\d+(\.\d+)?)|\.)").unwrap());

#[derive(PartialEq, Debug)]
enum Segment<'a> {
    Seg(&'a str, f64),
    Last(&'a str),
}

impl Ord for Segment<'_> {
    fn cmp(&self, other: &Self) -> Ordering {
        match (self, other) {
            (Seg(ss, sd), Seg(os, od)) => {
                let c = ss.cmp(os);
                if c != Ordering::Equal {
                    c
                } else {
                    // We know sd and od are finite numbers, so ordering is sane and we do only
                    // care about true equality.
                    #[allow(clippy::float_cmp)]
                    if sd == od {
                        Ordering::Equal
                    } else if sd > od {
                        Ordering::Greater
                    } else {
                        Ordering::Less
                    }
                }
            }
            (Seg(ss, _), Last(os)) => ss.cmp(os).then(Ordering::Greater),
            (Last(ss), Last(os)) => ss.cmp(os),
            (Last(ss), Seg(os, _)) => ss.cmp(os).then(Ordering::Less),
        }
    }
}

impl PartialOrd for Segment<'_> {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Eq for Segment<'_> {}

#[self_referencing]
#[derive(Eq, PartialEq, Debug)]
pub struct ParsedString {
    data: String,
    #[borrows(data)]
    #[covariant]
    segs: Vec<Segment<'this>>,
}

// This makes many tiny, short-lives allocations.
// Using rental/ouroboros it's possible to eliminate them for a ~20% speed increase. Raw unsafe
// pointers may be faster still.
// Using one allocation and storing indices will also work at a reduced benefit.
#[must_use]
pub fn key(s: &str) -> ParsedString {
    let s = s.to_lowercase();

    ParsedStringBuilder {
        data: s,
        segs_builder: |s| {
            let mut i = 0;
            let mut segs = Vec::new();
            for c in SEGMENT_RE.captures_iter(s) {
                let s = c.get(1).expect("Invalid capture").as_str();
                let ds = c.get(2).expect("Invalid capture").as_str();
                i = c.get(0).expect("Invalid capture").end();
                let seg = if ds == "." {
                    Seg(s, 0.0)
                } else if let Ok(d) = ds.parse::<f64>() {
                    if d.is_finite() {
                        Seg(s, d)
                    } else {
                        Seg(c.get(0).expect("Invalid capture").as_str(), 0.0)
                    }
                } else {
                    Seg(c.get(0).expect("Invalid capture").as_str(), 0.0)
                };

                segs.push(seg);
            }

            let last = &s[i..];
            segs.push(Last(last));
            segs
        },
    }
    .build()
}

impl Ord for ParsedString {
    fn cmp(&self, other: &Self) -> Ordering {
        for (a, b) in self.borrow_segs().iter().zip(other.borrow_segs().iter()) {
            let c = a.cmp(b);
            if c != Ordering::Equal {
                return c;
            }
        }

        self.borrow_segs().len().cmp(&other.borrow_segs().len())
    }
}

impl PartialOrd for ParsedString {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

#[cfg(test)]
mod tests {
    use std::cmp::Ordering;

    use super::key;

    fn compare(a: &str, b: &str) -> Ordering {
        let a = key(a);
        let b = key(b);
        println!("{:?}, {:?}, {:?}", a, b, a.cmp(&b));
        a.cmp(&b)
    }

    fn eq(a: &str, b: &str) {
        assert_eq!(compare(a, b), Ordering::Equal)
    }

    fn lt(a: &str, b: &str) {
        assert_eq!(compare(a, b), Ordering::Less);
        assert_eq!(compare(b, a), Ordering::Greater);
    }

    #[test]
    fn no_numbers() {
        eq("a", "a");
        lt("a", "b");
        lt("abc", "abcd");
        lt("abc", "abd");
        lt("ABC", "abd");
        lt("aBC", "Abd");
        lt("aBc", "AbD");
        eq("ABC", "abc");
        eq("abc", "ABC");
        lt("", "ABC");
    }

    #[test]
    fn only_numbers() {
        eq("17", "17");
        lt("16", "16.5");
        lt("4", "5");
        lt("16.7", "17");
    }

    #[test]
    fn combined() {
        eq("abc 10 abc 20", "abc 10 abc 20");
        lt("abc 10 abc 16", "abc 10 abc 16.5");
        lt("abc 10 abc 18", "abc 10 abd 17");
    }

    #[test]
    fn int_fail_case() {
        // This case fails when integer based tokenization is used.
        lt("16:", "16.5:");
    }

    #[test]
    fn sort_order() {
        lt("0a1f935e99.jpg", "01_2.jpg");
        lt("0a1f935e99.jpg", "bmidtl.jpg");
        lt("abcd", "abcd01");
    }

    #[test]
    fn unicode() {
        eq("K", "K"); // Kelvin sign
        lt("あ", "い");
        lt("あ", "雨");
        // Would require Mecab to sort these properly
        // lt("雨", "い");
        // lt("い", "ア");
        // lt("あ", "ア");
    }

    #[test]
    fn sort_no_number_before_number() {
        lt("m.png", "m2.png")
    }

    #[test]
    fn sort_chapters() {
        lt("ch 100.zip", "ch 100.5.zip")
    }

    #[test]
    fn example_files() {
        // From http://davekoelle.com/alphanum.html plus some additions
        let mut unsorted = vec![
            "z1.doc",
            "z10.doc",
            "z100.5.doc",
            "z100.eoc",
            "z101.doc",
            "z102.doc",
            "z11.doc",
            "z12.doc",
            "z13.doc",
            "z14.doc",
            "z15.doc",
            "z16.doc",
            "z17.doc",
            "z18.doc",
            "z19.DOC",
            "z2.doc",
            "Z20.doc",
            "a3.doc",
            "z4.doc",
            "z4.5.doc",
            "z4.3.doc",
            "z4.75.doc",
            "z4.7.doc",
            "Z5.doc",
            "B6.DOC",
            "z7.doc",
            "c8.doc",
            "z9.doc",
        ];

        let sorted = vec![
            "a3.doc",
            "B6.DOC",
            "c8.doc",
            "z1.doc",
            "z2.doc",
            "z4.doc",
            "z4.3.doc",
            "z4.5.doc",
            "z4.7.doc",
            "z4.75.doc",
            "Z5.doc",
            "z7.doc",
            "z9.doc",
            "z10.doc",
            "z11.doc",
            "z12.doc",
            "z13.doc",
            "z14.doc",
            "z15.doc",
            "z16.doc",
            "z17.doc",
            "z18.doc",
            "z19.DOC",
            "Z20.doc",
            "z100.eoc",
            "z100.5.doc",
            "z101.doc",
            "z102.doc",
        ];

        unsorted.sort_by_cached_key(|s| key(s));
        assert_eq!(unsorted, sorted);
    }
}
