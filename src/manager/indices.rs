use std::cell::Ref;
use std::cmp::{min, Ordering};
use std::collections::VecDeque;
use std::fmt;
use std::ops::RangeInclusive;

use derive_more::{Add, AddAssign, Deref, Sub, SubAssign};
use Indices::*;

use super::archive::Archive;
use super::Archives;
use crate::com::Direction::{self, *};

#[derive(
    Debug, Deref, PartialEq, PartialOrd, Eq, Ord, Clone, Copy, Add, Sub, AddAssign, SubAssign,
)]
pub struct AI(pub usize);
#[derive(Debug, PartialEq, PartialOrd, Eq, Ord, Clone, Copy, Add, Sub, AddAssign, SubAssign)]
pub struct PI(pub usize);

#[derive(Debug, Eq, PartialEq, Clone, Copy)]
enum Indices {
    Normal(AI, PI),
    Empty(AI),
}

#[derive(Clone)]
// All contstructed page indices point to either a valid page or an empty archive.
pub(super) struct PageIndices {
    indices: Indices,
    archives: Archives,
}

impl Eq for PageIndices {}

impl PartialEq for PageIndices {
    fn eq(&self, other: &Self) -> bool {
        self.indices == other.indices
    }
}

impl fmt::Debug for PageIndices {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "PI: {:?}.{:?}", self.a(), self.p())
    }
}

impl Ord for PageIndices {
    fn cmp(&self, other: &Self) -> Ordering {
        if self.a() != other.a() {
            return self.a().cmp(&other.a());
        }
        match (self.indices, other.indices) {
            (Normal(sa, sp), Normal(oa, op)) => sa.cmp(&oa).then_with(|| sp.cmp(&op)),
            (Empty(_), Empty(_)) => Ordering::Equal,
            (Normal(..), Empty(_)) | (Empty(_), Normal(..)) => panic!(
                "Tried to compare two page indices for the same archive but only one was empty"
            ),
        }
    }
}

impl PartialOrd for PageIndices {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl PageIndices {
    pub(super) fn new(a: usize, p: Option<usize>, archives: Archives) -> Self {
        assert!(a < archives.borrow().len());
        let indices = if let Some(pi) = p {
            assert!(pi < archives.borrow().get(a).unwrap().page_count());
            Normal(AI(a), PI(pi))
        } else {
            Empty(AI(a))
        };

        Self { indices, archives }
    }

    pub const fn a(&self) -> AI {
        match self.indices {
            Normal(a, _) | Empty(a) => a,
        }
    }

    pub const fn p(&self) -> Option<PI> {
        match self.indices {
            Normal(_, p) => Some(p),
            Empty(_) => None,
        }
    }

    // Bumps the archive index by one when a new archive is added to the start of the queue.
    pub(super) fn increment_archive(&mut self) {
        match self.indices {
            Normal(a, p) => self.indices = Normal(a + AI(1), p),
            Empty(a) => self.indices = Empty(a + AI(1)),
        }
    }

    // Drops the archive index by one when an archive is dropped from the start of the queue.
    pub(super) fn decrement_archive(&mut self) {
        assert_ne!(self.a().0, 0);

        match self.indices {
            Normal(a, p) => self.indices = Normal(a - AI(1), p),
            Empty(a) => self.indices = Empty(a - AI(1)),
        }
    }

    fn add(&self, x: usize) -> Option<Self> {
        // p is, temporarily, not guaranteed to be a PI
        let (mut a, mut p) = match self.indices {
            Normal(a, p) => (a, p.0),
            Empty(a) => (a, 0),
        };

        let mut new = self.clone();
        p += x;

        for arch in self.archives.borrow().range(a.0..) {
            let pc = arch.page_count();
            if pc == 0 {
                if p == 0 {
                    new.indices = Empty(a);
                    return Some(new);
                }
                // Count broken or empty archives as containing one page.
                p -= 1;
            } else if p < pc {
                new.indices = Normal(a, PI(p));
                return Some(new);
            } else {
                p -= pc;
            }
            a += AI(1);
        }

        None
    }

    fn sub(&self, x: usize) -> Option<Self> {
        // p is, temporarily, not guaranteed to be a PI
        let (mut a, mut p) = match self.indices {
            Normal(a, p) => (a, p.0),
            Empty(a) => (a, 0),
        };

        let pc = self.page_count();
        let mut new = self.clone();

        if p >= x {
            if pc != 0 {
                new.indices = Normal(a, PI(p - x));
            } else {
                new.indices = Empty(a);
            }
            return Some(new);
        }

        for arch in self.archives.borrow().range(..a.0).rev() {
            assert_ne!(a.0, 0);

            a -= AI(1);

            let pc = arch.page_count();
            p += pc;
            if pc == 0 {
                // Count broken or empty archives as containing one page.
                p += 1;
            }

            if p >= x {
                if pc != 0 {
                    new.indices = Normal(a, PI(p - x));
                } else {
                    assert_eq!(p, x);
                    new.indices = Empty(a);
                }
                return Some(new);
            }
        }

        None
    }

    pub(super) fn try_move_pages(&self, d: Direction, mut n: usize) -> Option<Self> {
        if d == Absolute {
            let pc = self.page_count();
            if pc == 0 {
                return Some(self.clone());
            }
            // Users will enter one-indexed pages, but still accept 0 as a page
            n = n.saturating_sub(1);

            let p = min(n, pc - 1);
            let mut new = self.clone();
            new.indices = Normal(self.a(), PI(p));
            return Some(new);
        }

        match d {
            Absolute => unreachable!(),
            Forwards => self.add(n),
            Backwards => self.sub(n),
        }
    }

    pub(super) fn move_clamped_in_archive(&self, d: Direction, n: usize) -> Self {
        let out = self.try_move_pages(d, n);

        if let Some(out) = out {
            if out.a() == self.a() {
                return out;
            }
        }

        let a = self.a();
        let mut new = self.clone();
        new.indices = match self.page_count() {
            0 => Empty(a),
            len => match d {
                Absolute => unreachable!(),
                Forwards => Normal(a, PI(len - 1)),
                Backwards => Normal(a, PI(0)),
            },
        };
        new
    }

    pub(super) fn move_clamped(&self, d: Direction, n: usize) -> Self {
        let out = self.try_move_pages(d, n);

        if let Some(out) = out {
            return out;
        }

        match d {
            Absolute => unreachable!(),
            Forwards => Self::last(self.archives.clone()),
            Backwards => Self::first(self.archives.clone()),
        }
    }

    pub(super) fn wrapping_range<'a, 'b: 'a>(
        &'b self,
        range: RangeInclusive<isize>,
    ) -> WrappingPageIterator<'a> {
        // There's no valid use for a range that isn't centered on the current page.
        // Not yet anyway.
        assert!(range.start() <= &0);
        assert!(range.end() >= &0);

        WrappingPageIterator {
            initial: self.clone(),
            start: self.move_clamped(Backwards, range.start().unsigned_abs()),
            end: self.move_clamped(Forwards, range.end().unsigned_abs()),
            next: Some(self.clone()),
            forwards: true,
            _archives_ref: self.archives.borrow(),
        }
    }

    pub(super) fn wrapping_range_in_archive<'a, 'b: 'a>(
        &'b self,
        range: RangeInclusive<isize>,
    ) -> WrappingPageIterator<'a> {
        // There's no valid use for a range that isn't centered on the current page.
        // Not yet anyway.
        assert!(range.start() <= &0);
        assert!(range.end() >= &0);

        WrappingPageIterator {
            initial: self.clone(),
            start: self.move_clamped_in_archive(Backwards, range.start().unsigned_abs()),
            end: self.move_clamped_in_archive(Forwards, range.end().unsigned_abs()),
            next: Some(self.clone()),
            forwards: true,
            _archives_ref: self.archives.borrow(),
        }
    }

    // returns an iterator over the difference in the two ranges centered on self and other.
    pub(super) fn diff_range_with_new<'a, 'b: 'a>(
        &'b self,
        new: &Self,
        range: &RangeInclusive<isize>,
    ) -> Option<DiffPageRangeIterator<'a>> {
        if self == new {
            return None;
        }

        let (start, end) = if self > new {
            (
                new.try_move_pages(Forwards, range.end().unsigned_abs() + 1)?,
                self.move_clamped(Forwards, range.end().unsigned_abs()),
            )
        } else {
            (
                self.move_clamped(Backwards, range.start().unsigned_abs()),
                new.try_move_pages(Backwards, range.start().unsigned_abs() + 1)?,
            )
        };

        assert!(start <= end);

        let next = Some(start);
        Some(DiffPageRangeIterator {
            end,
            next,
            _archives_ref: self.archives.borrow(),
        })
    }

    fn page_count(&self) -> usize {
        self.archives
            .borrow()
            .get(self.a().0)
            .expect("PageIndices always point to existing archives")
            .page_count()
    }

    fn first(archives: Archives) -> Self {
        let arches = archives.borrow();
        let a = arches
            .get(0)
            .expect("Manager archives list should never be empty");

        let indices = match a.page_count() {
            0 => Empty(AI(0)),
            _ => Normal(AI(0), PI(0)),
        };
        drop(arches);

        Self { indices, archives }
    }

    fn last(archives: Archives) -> Self {
        let arches = archives.borrow();
        let a = arches
            .get(arches.len() - 1)
            .expect("Manager archives list should never be empty");

        let indices = match a.page_count() {
            0 => Empty(AI(arches.len() - 1)),
            pc => Normal(AI(arches.len() - 1), PI(pc - 1)),
        };
        drop(arches);

        Self { indices, archives }
    }
}

// Starts in the middle and goes forward, then backwards.
pub(super) struct WrappingPageIterator<'a> {
    initial: PageIndices,
    start: PageIndices,
    end: PageIndices,
    next: Option<PageIndices>,
    forwards: bool,
    // This iterator will become invalid if the archives change.
    // We must borrow the data to keep it from being mutated elsewhere.
    _archives_ref: Ref<'a, VecDeque<Archive>>,
}

impl<'a> Iterator for WrappingPageIterator<'a> {
    type Item = PageIndices;

    fn next(&mut self) -> Option<Self::Item> {
        let next;
        match &self.next {
            Some(n) => next = n.clone(),
            None => return None,
        }

        if self.forwards {
            self.next = next.try_move_pages(Forwards, 1);
            if let Some(p) = &self.next {
                if p <= &self.end {
                    return Some(next);
                }
            }
            self.forwards = false;

            self.next = self.initial.try_move_pages(Backwards, 1);
        } else {
            self.next = next.try_move_pages(Backwards, 1);
        }

        if let Some(p) = &self.next {
            if p >= &self.start {
                return Some(next);
            }
        }
        self.next = None;

        Some(next)
    }
}

pub(super) struct DiffPageRangeIterator<'a> {
    end: PageIndices,
    next: Option<PageIndices>,
    // This iterator will become invalid if the archives change.
    // We must borrow the data to keep it from being mutated elsewhere.
    _archives_ref: Ref<'a, VecDeque<Archive>>,
}

impl<'a> Iterator for DiffPageRangeIterator<'a> {
    type Item = PageIndices;

    fn next(&mut self) -> Option<Self::Item> {
        let next;
        match &self.next {
            Some(n) => next = n.clone(),
            None => return None,
        }

        if next < self.end {
            self.next = next.try_move_pages(Forwards, 1);
            assert!(self.next.is_some());
        } else {
            self.next = None;
        }
        Some(next)
    }
}
