use std::path::Path;

use gtk::gdk_pixbuf::Pixbuf;
use once_cell::sync::Lazy;


// Might be able to reconsider once the heif and jxl loaders fix their severe memory leaks, maybe
// that will stop the segfaults.
static BANNED_EXTENSIONS: [&str; 4] = ["heic", "heif", "avif", "jxl"];

static PIXBUF_EXTENSIONS: Lazy<Vec<String>> = Lazy::new(|| {
    Pixbuf::formats()
        .iter()
        .flat_map(gtk::gdk_pixbuf::PixbufFormat::extensions)
        .map(|e| e.to_string())
        .filter(|e| {
            for x in BANNED_EXTENSIONS {
                if e == x {
                    return false;
                }
            }
            true
        })
        .collect()
});

// TODO -- verify each of these.
static NATIVE_EXTENSIONS: [&str; 10] = [
    "jpg", "jpeg", "png", "bmp", "gif", "ico", "pbm", "pgm", "ppm", "tga",
];

pub fn is_supported_page_extension<P: AsRef<Path>>(path: P) -> bool {
    let e = match path.as_ref().extension() {
        Some(e) => e.to_string_lossy().to_string().to_lowercase(),
        None => return false,
    };

    // These are small arrays so hashing is probably not worth it.
    for n in NATIVE_EXTENSIONS {
        if e == n {
            return true;
        }
    }

    for p in PIXBUF_EXTENSIONS.iter() {
        if e == *p {
            return true;
        }
    }

    false
}

pub fn is_natively_supported_image<P: AsRef<Path>>(path: P) -> bool {
    let e = match path.as_ref().extension() {
        Some(e) => e.to_string_lossy().to_string().to_lowercase(),
        None => return false,
    };

    // These are small arrays so hashing is probably not worth it.
    for n in NATIVE_EXTENSIONS {
        if e == n {
            return true;
        }
    }
    false
}

pub fn is_pixbuf_extension<P: AsRef<Path>>(path: P) -> bool {
    let e = match path.as_ref().extension() {
        Some(e) => e.to_string_lossy().to_string().to_lowercase(),
        None => return false,
    };

    // These are small arrays so hashing is probably not worth it.
    for p in PIXBUF_EXTENSIONS.iter() {
        if e == *p {
            return true;
        }
    }
    false
}

// Probing each archive would be unreasonably slow.
const ARCHIVE_FORMATS: [&str; 13] = [
    "zip", "cbz", "rar", "cbr", "7z", "cb7z", "tar", "pax", "gz", "bz2", "zst", "lz4", "xz",
];

pub fn is_archive_path<P: AsRef<Path>>(path: P) -> bool {
    let ext = match path.as_ref().extension() {
        Some(e) => e.to_string_lossy().to_string().to_lowercase(),
        None => return false,
    };

    for x in ARCHIVE_FORMATS {
        if x == ext {
            return true;
        }
    }
    false
}
