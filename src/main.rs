#[macro_use]
extern crate log;

// The tikv fork may not be easily buildable for Windows.
#[cfg(not(target_env = "msvc"))]
#[global_allocator]
static GLOBAL: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

use std::future::Future;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::pin::Pin;
use std::thread::{self, JoinHandle};

use gtk::glib;

use self::com::MAWithResponse;

mod elapsedlogger;

mod closing;
mod com;
mod config;
mod gui;
mod manager;
mod natsort;
mod pools;
#[allow(unused)]
mod resample;
mod socket;
mod unrar;

fn spawn_thread<F, T>(name: &str, f: F) -> JoinHandle<T>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    thread::Builder::new()
        .name(name.to_string())
        .spawn(f)
        .unwrap_or_else(|_| panic!("Error spawning thread {}", name))
}

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
type Fut<T> = Pin<Box<dyn Future<Output = T>>>;

fn main() {
    #[cfg(target_family = "unix")]
    unsafe {
        // This sets a restrictive umask to prevent other users from reading anything written by
        // this program. Images can be private and sockets can be used to run arbitrary
        // executables.
        libc::umask(0o077);
        // Tune memory trimming, otherwise the resident memory set tends to explode in size. The
        // default behaviour is dynamic and seems very poorly tuned for applications like an
        // image viewer.
        #[cfg(target_env = "gnu")]
        libc::mallopt(libc::M_TRIM_THRESHOLD, 128 * 1024);
    }

    elapsedlogger::init_logging();
    if !config::init() {
        return;
    }

    // Do this now so we can be certain it is initialized before any potential calls.
    gtk::init().expect("GTK could not be initialized");
    let (manager_sender, manager_receiver) = flume::unbounded::<MAWithResponse>();
    // PRIORITY_DEFAULT is enough to be higher priority than GTK redrawing events.
    let (gui_sender, gui_receiver) = glib::MainContext::channel(glib::PRIORITY_HIGH);

    closing::init(gui_sender.clone());

    let sock_handle = socket::init(&gui_sender);
    let man_handle = manager::run_manager(manager_receiver, gui_sender);

    if let Err(e) = catch_unwind(AssertUnwindSafe(|| gui::run(manager_sender, gui_receiver))) {
        // This will only happen on programmer error, but we want to make sure the manager thread
        // has time to exit and clean up temporary files.
        // The only things we do after this are cleanup.
        error!("gui::run panicked unexpectedly: {:?}", e);

        // This should _always_ be a no-op since it should have already been closed by a
        // CloseOnDrop.
        closing::close();
    }

    // These should never panic on their own, but they may if they're interacting with the gui
    // thread and it panics.
    if let Err(e) = catch_unwind(AssertUnwindSafe(|| {
        drop(man_handle.join());
    })) {
        error!("Joining manager thread panicked unexpectedly: {:?}", e);

        closing::close();
    }

    if let Some(h) = sock_handle {
        if let Err(e) = catch_unwind(AssertUnwindSafe(|| {
            drop(h.join());
        })) {
            error!("Joining socket thread panicked unexpectedly: {:?}", e);

            closing::close();
        }
    }
}
