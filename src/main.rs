#[macro_use]
extern crate log;

use std::future::Future;
use std::pin::Pin;
use std::thread::{self, JoinHandle};

use com::MAWithResponse;
use gtk::glib;

mod elapsedlogger;

mod closing;
mod com;
mod config;
mod gui;
mod manager;
mod natsort;
mod pools;
mod socket;
mod unrar;

fn spawn_thread<F, T>(name: &str, f: F) -> JoinHandle<T>
where
    F: FnOnce() -> T,
    F: Send + 'static,
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
    // Do this now so we can be certain it is initialized before any potential calls.
    gtk::init().expect("GTK could not be initialized");
    if !config::init() {
        return;
    }
    let (manager_sender, manager_receiver) = flume::unbounded::<MAWithResponse>();
    // PRIORITY_DEFAULT is enough to be higher priority than GTK redrawing events.
    let (gui_sender, gui_receiver) = glib::MainContext::channel(glib::PRIORITY_HIGH);

    closing::init(gui_sender.clone());

    let sh = socket::init(&gui_sender);
    let h = manager::run_manager(manager_receiver, gui_sender);
    gui::run(manager_sender, gui_receiver);

    drop(h.join());
    if let Some(h) = sh {
        drop(h.join());
    }
}