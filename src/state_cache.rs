// This is for persistent settings or cached values that are too fluid to be set in a config
// file, like window position and size. They should persist between application instances when
// possible, but not to the extent that they cause breakages (windows should remain on-screen).

use std::fs;
use std::path::PathBuf;

use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};

use crate::com::Res;

#[derive(Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct State {
    pub size: Res,
    // pub position: (i32, i32),
    pub maximized: bool,
}

static CACHE_FILE: Lazy<Option<PathBuf>> = Lazy::new(|| {
    let mut cache = dirs::cache_dir()?;
    cache.push("aw-man");
    cache.push("saved.json");
    Some(cache)
});

pub static STATE: Lazy<Option<State>> = Lazy::new(|| {
    let cache = (*CACHE_FILE).as_ref()?;
    if !cache.is_file() {
        return None;
    }

    let bytes = match fs::read(cache) {
        Ok(b) => b,
        Err(e) => {
            error!("File {cache:?} exists but could not be read: {e}");
            return None;
        }
    };

    match serde_json::from_slice(&bytes) {
        Ok(s) => Some(s),
        Err(e) => {
            error!("File {cache:?} exists but could not be parsed: {e}");
            None
        }
    }
});

pub fn save_settings(s: State) {
    if Some(&s) == STATE.as_ref() {
        return;
    }

    let Some(cache) = (*CACHE_FILE).as_ref() else { return };

    let serialized = serde_json::to_string(&s).unwrap();

    if let Some(p) = cache.parent() {
        if let Err(e) = fs::create_dir_all(p) {
            error!("Unable to create directory to save window state: {e}");
            return;
        }
    }

    if let Err(e) = fs::write(cache, serialized) {
        error!("Could not save window state: {e}");
    }

    trace!("Wrote window state cache for next run.");
}
