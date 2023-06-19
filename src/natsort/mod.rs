use core::fmt;
use std::cmp::Ordering;
use std::ffi::{OsStr, OsString};

use ouroboros::self_referencing;
use regex::Regex;
use Segment::*;

thread_local! {
    static SEGMENT_RE: Regex = Regex::new(r"([^\d.]*)((\d+(\.\d+)?)|\.)").unwrap();
}

#[derive(PartialEq, Debug)]
enum Segment<'a> {
    Seg(&'a str, f64),
    Last(&'a str),
}

impl Ord for Segment<'_> {
    fn cmp(&self, other: &Self) -> Ordering {
        match (self, other) {
            (Seg(ss, sd), Seg(os, od)) => ss.cmp(os).then_with(|| sd.total_cmp(od)),
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
pub struct ParsedString {
    original: OsString,
    lowercase: String,
    #[borrows(lowercase)]
    #[covariant]
    segs: Vec<Segment<'this>>,
}

#[must_use]
pub fn key(s: &OsStr) -> ParsedString {
    let original = s.to_owned();
    let lowercase = original.to_string_lossy().to_lowercase();

    ParsedString::from_strings(original, lowercase)
}

impl From<OsString> for ParsedString {
    fn from(original: OsString) -> Self {
        let lowercase = original.to_string_lossy().to_lowercase();

        Self::from_strings(original, lowercase)
    }
}

impl Ord for ParsedString {
    fn cmp(&self, other: &Self) -> Ordering {
        for (a, b) in self.borrow_segs().iter().zip(other.borrow_segs().iter()) {
            let c = a.cmp(b);
            if c != Ordering::Equal {
                return c;
            }
        }

        // This check could be done first, but comparing equal items should be rare
        self.borrow_original().cmp(other.borrow_original())
    }
}

impl PartialOrd for ParsedString {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Eq for ParsedString {}

impl PartialEq for ParsedString {
    fn eq(&self, other: &Self) -> bool {
        self.borrow_original() == other.borrow_original()
    }
}

impl fmt::Debug for ParsedString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ParsedString")
            .field("original", &self.borrow_original())
            .field("segments", &self.borrow_segs())
            .finish()
    }
}

impl ParsedString {
    #[must_use]
    pub fn into_original(self) -> OsString {
        self.into_heads().original
    }

    fn from_strings(original: OsString, lowercase: String) -> Self {
        ParsedStringBuilder {
            original,
            lowercase,
            segs_builder: |s| {
                let mut i = 0;
                let mut segs = Vec::new();
                SEGMENT_RE.with(|r| {
                    for c in r.captures_iter(s) {
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
                });

                let last = &s[i..];
                segs.push(Last(last));
                segs
            },
        }
        .build()
    }
}

#[cfg(test)]
mod tests {
    use std::cmp::Ordering;
    use std::ffi::OsStr;

    use super::key;

    fn compare(a: &str, b: &str) -> Ordering {
        let a = key(OsStr::new(a));
        let b = key(OsStr::new(b));
        println!("{a:?}, {b:?}, {:?}", a.cmp(&b));
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
        lt("", "ABC");
    }

    #[test]
    fn case_change() {
        lt("A", "a");
        lt("ABC", "abc");
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
    fn octal_parse() {
        // We should be treating everything as decimal
        lt("12", "013");
        // The tie should be broken in this order since 0 < [1-9].
        lt("05", "5");
        lt("012", "12");
        lt("09", "9");
    }

    #[test]
    fn sort_order() {
        lt("0a1f935e99.jpg", "01_2.jpg");
        lt("0a1f935e99.jpg", "bmidtl.jpg");
        lt("abcd", "abcd01");
    }

    #[test]
    fn unicode() {
        // Kelvin sign
        lt("J", "K");
        lt("K", "K");
        lt("K", "L");
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

        unsorted.sort_by_cached_key(|s| key(OsStr::new(s)));
        assert_eq!(unsorted, sorted);
    }
}
