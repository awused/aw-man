// COPYRIGHT https://github.com/gtk-rs/gtk4-rs/blob/main/COPYRIGHT
//
// The gtk-rs Project is licensed under the MIT license, see the LICENSE file or
// <http://opensource.org/licenses/MIT>.
//
// Copyrights in the gtk-rs Project project are retained by their contributors.
// No copyright assignment is required to contribute to the gtk-rs Project
// project.
//
// For full authorship information, see the version control history.
//
// This project provides interoperability with various GNOME libraries but
// doesn't distribute any parts of them. Distributing compiled libraries and
// executables that link to those libraries may be subject to terms of the GNU
// LGPL or other licenses. For more information check the license of each GNOME
// library.
//
// See https://github.com/gtk-rs/gtk4-rs/commit/bf5186fe730407db387fa09482a6f2c86981abe8
//

pub(super) fn epoxy_library() -> &'static libloading::Library {
    use std::sync::OnceLock;

    static EPOXY: OnceLock<libloading::Library> = OnceLock::new();

    EPOXY.get_or_init(|| {
        #[cfg(target_os = "macos")]
        let filename = "libepoxy.0.dylib";
        #[cfg(all(unix, not(target_os = "macos")))]
        let filename = "libepoxy.so.0";
        #[cfg(windows)]
        let filename = "libepoxy-0.dll";

        // SAFETY: the filename is a compile-time constant.
        let library = unsafe { libloading::Library::new(filename) };

        #[cfg(windows)]
        let library = library.or_else(|_| unsafe { libloading::Library::new("epoxy-0.dll") });

        library.unwrap()
    })
}

/// `epoxy_<name>` symbols are entries in libepoxy's dispatch table, not
/// functions, so they must be dereferenced once to obtain the actual
/// dispatcher for the currently bound GL context.
pub(super) fn get_proc_address(name: &str) -> *const std::ffi::c_void {
    use std::ffi::c_void;

    let library = epoxy_library();
    let symbol = format!("epoxy_{name}");

    unsafe {
        library.get::<*const c_void>(&symbol).map_or(std::ptr::null(), |sym| {
            let entry = sym.try_as_raw_ptr().unwrap() as *const *const c_void;
            *entry
        })
    }
}
