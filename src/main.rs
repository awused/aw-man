#![cfg_attr(not(feature = "windows-console"), windows_subsystem = "windows")]

#[macro_use]
extern crate tracing;

// The tikv fork may not be easily buildable for Windows.
// Disabled on mac os for now, maybe permanently.
// #[cfg(not(target_env = "msvc"))]
#[cfg(target_os = "linux")]
#[global_allocator]
static GLOBAL: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

use std::future::Future;
use std::panic::catch_unwind;
use std::pin::Pin;
use std::thread::{self, JoinHandle};

use config::OPTIONS;
use gtk::Settings;
use manager::files::print_formats;
use pools::downscaling::print_gpus;

mod closing;
mod com;
mod config;
mod elapsedlogger;
mod gui;
mod manager;
mod natsort;
mod pools;
mod resample;
mod socket;
mod state_cache;
mod unrar;

fn spawn_thread<F, T>(name: &str, f: F) -> JoinHandle<T>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    thread::Builder::new()
        .name(name.to_string())
        .spawn(f)
        .unwrap_or_else(|e| panic!("Error spawning thread {name}: {e}"))
}

type Fut<T> = Pin<Box<dyn Future<Output = T>>>;

fn main() {
    elapsedlogger::init_logging();
    color_eyre::install().unwrap();

    // GTK sure is a great piece of software
    let old_debug = std::env::var("GTK_DEBUG").unwrap_or_default();
    if old_debug.is_empty() {
        std::env::set_var("GDK_DEBUG", "gl-prefer-gl");
    } else {
        std::env::set_var("GDK_DEBUG", old_debug + ",gl-prefer-gl");
    }

    #[cfg(target_family = "unix")]
    unsafe {
        // This sets a restrictive umask to prevent other users from reading anything written by
        // this program. Images can be private and sockets can be used to run arbitrary
        // executables.
        libc::umask(0o077);
        // Tune glibc memory trimming, otherwise the resident memory set tends to explode in size.
        // The default behaviour is dynamic and seems very poorly tuned for applications like an
        // image viewer, for some reason.
        // Do not tune jemalloc beyond what is already configured through build-time settings.
        #[cfg(all(target_env = "gnu", not(target_os = "linux")))]
        libc::mallopt(libc::M_TRIM_THRESHOLD, 128 * 1024);
    }

    config::init();

    if OPTIONS.show_supported {
        return print_formats();
    } else if OPTIONS.show_gpus {
        return print_gpus();
    }

    let (manager_sender, manager_receiver) = flume::unbounded();
    let (gui_sender, gui_receiver) = flume::unbounded();

    closing::init(gui_sender.clone());

    let sock_handle = socket::init(&gui_sender);
    let man_handle = manager::run(manager_receiver, gui_sender);


    // All GTK calls that could possibly be reached before this completes (pixbuf, channel sends)
    // are safe to call off the main thread and before GTK is initialzed.
    gtk::init().expect("GTK could not be initialized");

    // No one should ever have this disabled
    Settings::default().unwrap().set_gtk_hint_font_metrics(true);

    if let Err(e) = catch_unwind(|| gui::run(manager_sender, gui_receiver)) {
        // This will only happen on programmer error, but we want to make sure the manager thread
        // has time to exit and clean up temporary files.
        // The only things we do after this are cleanup.
        closing::fatal(format!("gui::run panicked unexpectedly: {e:?}"));
    }
    closing::close();

    // These should never panic on their own, but they may if they're interacting with the gui
    // thread and it panics.
    if let Err(e) = man_handle.join() {
        closing::fatal(format!("Joining manager thread panicked unexpectedly: {e:?}"));
    }

    if let Some(h) = sock_handle {
        if let Err(e) = h.join() {
            closing::fatal(format!("Joining socket thread panicked unexpectedly: {e:?}"));
        }
    }
}
