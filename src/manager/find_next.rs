use std::cmp::{Ordering, Reverse};
use std::collections::BinaryHeap;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

use once_cell::sync::Lazy;
use rayon::iter::{ParallelBridge, ParallelIterator};
use regex::Regex;

use crate::manager::files::is_archive_path;
use crate::natsort;


// This is for compatibility with manga-syncer
static MANGA_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"^(Vol\. [^ ]+ )?Ch\. (([^ a-zA-Z]+)[a-zA-Z]?) (.* )?- [a-zA-Z0-9_-]+\.[a-z]{0,3}$")
        .unwrap()
});

pub(super) struct SortKey {
    chapter: Option<f64>,
    nkey: natsort::ParsedString,
}

impl Ord for SortKey {
    fn cmp(&self, other: &Self) -> Ordering {
        // Could potentially do something more involved with volume numbers, but not yet a problem.
        match (self.chapter, other.chapter) {
            (Some(sc), Some(oc)) => sc.total_cmp(&oc),
            // Put archives with no known chapter after those with chapters.
            (Some(_), None) => Ordering::Less,
            (None, Some(_)) => Ordering::Greater,
            (None, None) => Ordering::Equal,
        }
        .then_with(|| self.nkey.cmp(&other.nkey))
    }
}

impl PartialOrd for SortKey {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl PartialEq for SortKey {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other).is_eq()
    }
}

impl Eq for SortKey {}

// Only called when parent() exists, so file_name() also exists
#[allow(clippy::fallible_impl_from)]
impl From<PathBuf> for SortKey {
    fn from(path: PathBuf) -> Self {
        let mut chapter = None;
        if let Some(cap) = MANGA_RE.captures(&path.file_name().unwrap().to_string_lossy()) {
            let d = cap[3].parse::<f64>();
            if let Ok(d) = d {
                chapter = Some(d);
            }
        }
        let nkey = OsString::from(path).into();

        Self { chapter, nkey }
    }
}

// Cache results so that we don't need to hit the file system multiple times for long jumps.
pub(super) enum SortKeyCache {
    Empty,
    // Don't bother sorting the results if we only need them once
    Unsorted(Vec<SortKey>),
    BackwardsHeap(BinaryHeap<SortKey>),
    ForwardsHeap(BinaryHeap<Reverse<SortKey>>),
}

impl SortKeyCache {
    fn heapify(self, ord: Ordering) -> Self {
        if let Self::Unsorted(v) = self {
            match ord {
                Ordering::Less => Self::BackwardsHeap(v.into_iter().collect()),
                Ordering::Greater => Self::ForwardsHeap(v.into_iter().map(Reverse).collect()),
                Ordering::Equal => unreachable!(),
            }
        } else {
            self
        }
    }

    fn pop(mut self) -> Option<(PathBuf, Self)> {
        let sk = match &mut self {
            Self::Empty | Self::Unsorted(_) => unreachable!(),
            Self::ForwardsHeap(h) => h.pop().map(|r| r.0),
            Self::BackwardsHeap(h) => h.pop(),
        };

        sk.map(|sk| (sk.nkey.into_original().into(), self))
    }
}

pub(super) fn for_path<P: AsRef<Path>>(
    path: P,
    ord: Ordering,
    mut cache: SortKeyCache,
) -> Option<(PathBuf, SortKeyCache)> {
    if let SortKeyCache::Empty = cache {
    } else {
        cache = cache.heapify(ord);
        return cache.pop();
    }

    let path = path.as_ref();

    let parent = path.parent()?;

    let start_key = path.to_owned().into();

    let mut unsorted: Vec<_> = fs::read_dir(parent)
        .ok()?
        .par_bridge()
        .filter_map(|de| {
            let depath = de.ok()?.path();
            if !is_archive_path(&depath) {
                return None;
            }

            let key: SortKey = depath.into();

            if key.cmp(&start_key) != ord {
                return None;
            }
            Some(key)
        })
        .collect();


    let (i, _) = if ord == Ordering::Greater {
        unsorted.iter().enumerate().min_by(|(_, a), (_, b)| a.cmp(b))?
    } else {
        unsorted.iter().enumerate().max_by(|(_, a), (_, b)| a.cmp(b))?
    };

    let next = unsorted.swap_remove(i);
    let path: PathBuf = next.nkey.into_original().into();
    debug!("Opening next archive {:?}", path.file_name(),);
    Some((path, SortKeyCache::Unsorted(unsorted)))
}
