use std::cell::{Ref, RefMut};
use std::cmp::{Ordering, max, min};
use std::collections::VecDeque;
use std::fmt;
use std::ops::{Deref, RangeInclusive};

use Indices::*;
use derive_more::{Add, AddAssign, Deref, Sub, SubAssign};

use super::Archives;
use super::archive::Archive;
use crate::com::Direction::{self, *};
use crate::com::OneOrTwo::{self, One, Two};

#[derive(
    Debug, Deref, PartialEq, PartialOrd, Eq, Ord, Clone, Copy, Add, Sub, AddAssign, SubAssign,
)]
pub struct AI(pub usize);
#[derive(Debug, PartialEq, PartialOrd, Eq, Ord, Clone, Copy, Add, Sub, AddAssign, SubAssign)]
pub struct PI(pub usize);

#[derive(Debug)]
pub(super) enum CurrentIndices {
    Single(PageIndices),
    // Dual(One) means we're intentionally displaying one page despite being in dual page mode, as
    // opposed to Single which, in dual page mode, will be changed to Dual(One) or Dual(Two) by
    // adjust_current_for_dual_page().
    Dual(OneOrTwo<PageIndices>),
}

impl Deref for CurrentIndices {
    type Target = PageIndices;

    fn deref(&self) -> &Self::Target {
        match self {
            Self::Single(c) | Self::Dual(One(c) | Two(c, _)) => c,
        }
    }
}

impl CurrentIndices {
    pub(super) fn increment_archive(&mut self) {
        match self {
            Self::Single(c) | Self::Dual(One(c)) => c.increment_archive(),
            Self::Dual(Two(c, n)) => {
                c.increment_archive();
                n.increment_archive();
            }
        }
    }

    pub(super) fn decrement_archive(&mut self) {
        match self {
            Self::Single(c) | Self::Dual(One(c)) => c.decrement_archive(),
            Self::Dual(Two(c, n)) => {
                c.decrement_archive();
                n.decrement_archive();
            }
        }
    }
}

#[derive(Debug, Eq, PartialEq, Clone, Copy)]
enum Indices {
    Normal(AI, PI),
    Empty(AI),
}

// All constructed page indices point to either a valid page or an empty archive.
#[derive(Clone)]
pub(super) struct PageIndices {
    indices: Indices,
    // TODO -- consider removing this and making PageIndices Copy
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
        match (self.indices, other.indices) {
            (Normal(sa, sp), Normal(oa, op)) => sa.cmp(&oa).then_with(|| sp.cmp(&op)),
            (Empty(sa), Empty(oa)) => sa.cmp(&oa),
            (Normal(sa, _), Empty(oa)) | (Empty(sa), Normal(oa, _)) => {
                sa.cmp(&oa).then_with(|| {
                    panic!(
                        "Tried to compare two page indices for the same archive but only one was \
                         empty. {self:?} {other:?}"
                    )
                })
            }
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
            assert!(pi < archives.borrow()[a].page_count());
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

    pub(super) fn archive(&self) -> Ref<Archive> {
        Ref::map(self.archives.borrow(), |archives| &archives[self.a().0])
    }

    pub(super) fn archive_mut(&self) -> RefMut<Archive> {
        RefMut::map(self.archives.borrow_mut(), |archives| &mut archives[self.a().0])
    }

    pub(super) fn unload(&self) {
        match self.indices {
            Normal(a, p) => self.archives.borrow()[a.0].unload(p),
            Empty(_) => (),
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

        let pc = self.archive().page_count();
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

    pub(super) fn try_move_pages(&self, d: Direction, n: usize) -> Option<Self> {
        match d {
            Forwards => self.add(n),
            Backwards => self.sub(n),
            Absolute => {
                let pc = self.archive().page_count();
                if pc == 0 {
                    return Some(self.clone());
                }

                let p = min(n, pc - 1);
                let mut new = self.clone();
                new.indices = Normal(self.a(), PI(p));
                Some(new)
            }
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
        new.indices = match self.archive().page_count() {
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
        // This value will not be returned
        initial: &Self,
    ) -> WrappingPageIterator<'a> {
        // There's no valid use for a range that isn't centered on the current page.
        // Not yet anyway.
        assert!(range.start() <= &0);
        assert!(range.end() >= &0);
        let start = self.move_clamped(Backwards, range.start().unsigned_abs());
        let end = self.move_clamped(Forwards, range.end().unsigned_abs());
        assert!(initial >= &start);
        assert!(initial <= &end);

        WrappingPageIterator {
            center: self.clone(),
            start,
            end,
            next: Some(initial.clone()),
            forwards: true,
            _archives_ref: self.archives.borrow(),
        }
    }

    pub(super) fn wrapping_range_in_archive<'a, 'b: 'a>(
        &'b self,
        range: RangeInclusive<isize>,
        // This value will not be returned
        initial: &Self,
    ) -> WrappingPageIterator<'a> {
        // There's no valid use for a range that isn't centered on the current page.
        // Not yet anyway.
        assert!(range.start() <= &0);
        assert!(range.end() >= &0);
        let start = self.move_clamped_in_archive(Backwards, range.start().unsigned_abs());
        let end = self.move_clamped_in_archive(Forwards, range.end().unsigned_abs());
        assert!(initial >= &start);
        assert!(initial <= &end);

        WrappingPageIterator {
            center: self.clone(),
            start,
            end,
            next: Some(initial.clone()),
            forwards: true,
            _archives_ref: self.archives.borrow(),
        }
    }

    // Returns an iterator over the difference in the two ranges centered on self and other.
    pub(super) fn diff_range_with_new<'a, 'b: 'a>(
        &'b self,
        new: &Self,
        range: &RangeInclusive<isize>,
    ) -> Option<DiffPageRangeIterator<'a>> {
        if self == new {
            return None;
        }

        let (start, end) = if new > self {
            // Moving forwards
            let before_new_start =
                new.try_move_pages(Backwards, range.start().unsigned_abs() + 1)?;
            let old_end = self.move_clamped(Forwards, range.end().unsigned_abs());
            (
                self.move_clamped(Backwards, range.start().unsigned_abs()),
                min(before_new_start, old_end),
            )
        } else {
            // Moving backwards
            let after_new_end = new.try_move_pages(Forwards, range.end().unsigned_abs() + 1)?;
            let old_start = self.move_clamped(Backwards, range.start().unsigned_abs());
            (
                max(after_new_end, old_start),
                self.move_clamped(Forwards, range.end().unsigned_abs()),
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

    pub(super) fn first(archives: Archives) -> Self {
        let arches = archives.borrow();
        let a = &arches[0];

        let indices = match a.page_count() {
            0 => Empty(AI(0)),
            _ => Normal(AI(0), PI(0)),
        };
        drop(arches);

        Self { indices, archives }
    }

    pub(super) fn last(archives: Archives) -> Self {
        let arches = archives.borrow();
        let a = &arches[arches.len() - 1];

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
    center: PageIndices,
    start: PageIndices,
    end: PageIndices,
    next: Option<PageIndices>,
    forwards: bool,
    // This iterator will become invalid if the archives change.
    // We must borrow the data to keep it from being mutated elsewhere.
    _archives_ref: Ref<'a, VecDeque<Archive>>,
}

impl Iterator for WrappingPageIterator<'_> {
    type Item = PageIndices;

    fn next(&mut self) -> Option<Self::Item> {
        let next = self.next.as_ref()?.clone();

        if self.forwards {
            self.next = next.try_move_pages(Forwards, 1);
            if let Some(p) = &self.next {
                if p <= &self.end {
                    return Some(next);
                }
            }
            self.forwards = false;

            self.next = self.center.try_move_pages(Backwards, 1);
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

impl Iterator for DiffPageRangeIterator<'_> {
    type Item = PageIndices;

    fn next(&mut self) -> Option<Self::Item> {
        let next = self.next.as_ref()?.clone();

        if next < self.end {
            self.next = next.try_move_pages(Forwards, 1);
            assert!(self.next.is_some());
        } else {
            self.next = None;
        }
        Some(next)
    }
}
