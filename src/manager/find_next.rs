use std::cmp::{Ordering, Reverse};
use std::collections::BinaryHeap;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

use once_cell::sync::Lazy;
use regex::Regex;

use crate::manager::files::is_archive_path;
use crate::natsort;


// This is for compatibility with manga-syncer
// TODO -- really consider just changing the names manga-syncer uses to something more sortable.
static MANGA_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"\\|/(Vol\. [^ ]+ )?Ch\. ([^ ]+) (.* )?- [a-zA-Z0-9_-]+\.[a-z]{0,3}$").unwrap()
});

pub(super) struct SortKey {
    chapter: Option<f64>,
    nkey: natsort::ParsedString,
}

impl Ord for SortKey {
    fn cmp(&self, other: &Self) -> Ordering {
        if let (Some(sc), Some(oc)) = (self.chapter, other.chapter) {
            if sc > oc {
                return Ordering::Greater;
            } else if sc < oc {
                return Ordering::Less;
            }
        }

        self.nkey.cmp(&other.nkey)
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

impl From<PathBuf> for SortKey {
    fn from(path: PathBuf) -> Self {
        let mut chapter = None;
        if let Some(cap) = MANGA_RE.captures(&path.to_string_lossy()) {
            let d = cap.get(2).expect("Invalid capture").as_str().parse::<f64>();
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
            SortKeyCache::Empty | SortKeyCache::Unsorted(_) => unreachable!(),
            SortKeyCache::ForwardsHeap(h) => h.pop().map(|r| r.0),
            SortKeyCache::BackwardsHeap(h) => h.pop(),
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
    let mut next = None;
    let mut unsorted = Vec::new();

    for de in fs::read_dir(parent).ok()? {
        let depath = match de {
            Ok(de) => de.path(),
            Err(_) => continue,
        };
        if !is_archive_path(&depath) {
            continue;
        }

        let key: SortKey = depath.into();

        if key.cmp(&start_key) != ord {
            continue;
        }

        if let Some(next_key) = &mut next {
            if key.cmp(next_key) != ord.reverse() {
                unsorted.push(key);
            } else {
                unsorted.push(std::mem::replace(next_key, key));
            }
        } else {
            next = Some(key);
        }
    }

    if let Some(SortKey { nkey, .. }) = next {
        let path: PathBuf = nkey.into_original().into();
        debug!("Opening next archive {:?}", path.file_name(),);
        Some((path, SortKeyCache::Unsorted(unsorted)))
    } else {
        None
    }
}
