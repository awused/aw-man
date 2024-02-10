use std::env::temp_dir;
use std::io::Write;
use std::marker::PhantomData;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::{process, thread};

use flume::{bounded, Receiver, Sender};
use once_cell::sync::{Lazy, OnceCell};

use crate::com::GuiAction;
use crate::spawn_thread;

type CloseSender = Mutex<Option<Sender<()>>>;
type CloseReceiver = Receiver<()>;

static CLOSED: Lazy<Arc<AtomicBool>> = Lazy::new(|| Arc::new(AtomicBool::new(false)));
static CLOSER: Lazy<(CloseSender, CloseReceiver)> = Lazy::new(|| {
    let (s, r) = bounded::<()>(0);
    (Mutex::new(Option::Some(s)), r)
});
static GUI_CLOSER: OnceCell<Sender<GuiAction>> = OnceCell::new();

#[derive(Default)]
pub struct CloseOnDrop {
    _phantom: PhantomData<Rc<CloseOnDrop>>,
}

// TODO -- https://github.com/rust-lang/rust/issues/68318
// impl !Send for CloseOnDrop {}
// impl !Sync for CloseOnDrop {}

impl Drop for CloseOnDrop {
    fn drop(&mut self) {
        if !closed() && !std::thread::panicking() {
            // This means something else panicked and at least one thread did not shut down cleanly.
            fatal(format!(
                "CloseOnDrop for {} was dropped without closing::close() being called.",
                thread::current().name().unwrap_or("unnamed")
            ));
        }
    }
}

pub fn closed() -> bool {
    CLOSED.load(Ordering::Relaxed)
}

pub async fn closed_fut() {
    // We only care that it's closed.
    let _ignored = CLOSER.1.recv_async().await;
}

pub fn close() -> bool {
    if !CLOSED.swap(true, Ordering::Relaxed) {
        let mut o = CLOSER.0.lock().expect("CLOSER lock poisoned");
        if o.is_some() {
            *o = Option::None;
        } else {
            error!("CLOSER unexpectedly closed before CLOSED");
        }
        if let Some(gc) = GUI_CLOSER.get() {
            drop(gc.send(GuiAction::Quit));
        }
        drop(o);
        true
    } else {
        false
    }
}

// Logs the error and closes the application.
// Saves the first fatal error to a crash log file in the system default temp directory.
pub fn fatal(msg: impl AsRef<str>) {
    let msg = msg.as_ref();

    error!("{msg}");

    if close() {
        let path = temp_dir().join(format!("aw-man_crash_{}", process::id()));
        let Ok(mut file) = std::fs::File::options().write(true).create_new(true).open(&path) else {
            error!("Couldn't open {path:?} for logging fatal error");
            return;
        };

        drop(file.write_all(msg.as_bytes()));
    }
}

pub fn init(gui_sender: Sender<GuiAction>) {
    Lazy::force(&CLOSER);

    GUI_CLOSER.set(gui_sender).expect("closing::init() called twice");

    #[cfg(target_family = "unix")]
    spawn_thread("signals", || {
        use std::os::raw::c_int;

        use signal_hook::consts::TERM_SIGNALS;
        use signal_hook::iterator::exfiltrator::SignalOnly;
        use signal_hook::iterator::SignalsInfo;

        let _cod = CloseOnDrop::default();

        if let Err(e) = catch_unwind(AssertUnwindSafe(|| {
            for sig in TERM_SIGNALS {
                // When terminated by a second term signal, exit with exit code 1.
                signal_hook::flag::register_conditional_shutdown(*sig, 1, CLOSED.clone())
                    .expect("Error registering signal handlers.");
            }

            let mut sigs: Vec<c_int> = Vec::new();
            sigs.extend(TERM_SIGNALS);
            let mut it = match SignalsInfo::<SignalOnly>::new(sigs) {
                Ok(i) => i,
                Err(e) => {
                    fatal(format!("Error registering signal handlers: {e:?}"));
                    return;
                }
            };

            if let Some(s) = it.into_iter().next() {
                info!("Received signal {s}, shutting down");
                close();
                it.handle().close();
            }
        })) {
            fatal(format!("Signal thread panicked unexpectedly: {e:?}"));
        };
    });

    #[cfg(windows)]
    spawn_thread("signals", || {
        ctrlc::set_handler(|| {
            if closed() {
                // When terminated by a second term signal, exit with exit code 1.
                std::process::exit(1);
            }

            info!("Received closing signal, shutting down");
            close();
        })
        .expect("Error registering signal handlers.");
    });
}
