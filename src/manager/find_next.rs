use std::cmp::Ordering;
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

struct SortKey {
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

fn get_key(path: &Path) -> SortKey {
    let nkey = natsort::key(path.as_os_str());
    let mut chapter = None;
    if let Some(cap) = MANGA_RE.captures(&path.to_string_lossy()) {
        let d = cap.get(2).expect("Invalid capture").as_str().parse::<f64>();
        if let Ok(d) = d {
            chapter = Some(d);
        }
    }

    SortKey { chapter, nkey }
}

pub(super) fn for_path<P: AsRef<Path>>(path: P, ord: Ordering) -> Option<PathBuf> {
    let path = path.as_ref();

    let parent = path.parent()?;

    let start_key = get_key(path);
    let mut next = None;
    let mut next_key = None;


    for de in fs::read_dir(parent).ok()? {
        let depath = match de {
            Ok(de) => de.path(),
            Err(_) => continue,
        };
        if !is_archive_path(&depath) {
            continue;
        }

        let key = get_key(&depath);

        if key.cmp(&start_key) != ord {
            continue;
        }

        if let Some(nk) = &next_key {
            if key.cmp(nk) != ord.reverse() {
                continue;
            }
        }

        next = Some(depath);
        next_key = Some(key);
    }

    if next.is_some() {
        debug!(
            "Opening next archive {:?}",
            next.as_ref().unwrap().file_name(),
        );
    }
    next
}
