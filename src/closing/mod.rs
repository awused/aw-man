use std::marker::PhantomData;
use std::os::raw::c_int;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

use flume::{bounded, Receiver, Sender};
use gtk::glib;
use once_cell::sync::{Lazy, OnceCell};
#[cfg(target_family = "unix")]
use signal_hook::iterator;

use crate::com::GuiAction;
use crate::spawn_thread;

type CloseSender = Mutex<Option<Sender<()>>>;
type CloseReceiver = Receiver<()>;

static CLOSED: Lazy<Arc<AtomicBool>> = Lazy::new(|| Arc::new(AtomicBool::new(false)));
static CLOSER: Lazy<(CloseSender, CloseReceiver)> = Lazy::new(|| {
    let (s, r) = bounded::<()>(0);
    (Mutex::new(Option::Some(s)), r)
});
static GUI_CLOSER: OnceCell<glib::Sender<GuiAction>> = OnceCell::new();

#[derive(Default)]
pub struct CloseOnDrop {
    _phantom: PhantomData<Rc<CloseOnDrop>>,
}

// TODO -- https://github.com/rust-lang/rust/issues/68318
// impl !Send for CloseOnDrop {}
// impl !Sync for CloseOnDrop {}

impl Drop for CloseOnDrop {
    fn drop(&mut self) {
        if !closed() {
            // This means something panicked and at least one thread did not shut down cleanly.
            error!(
                "CloseOnDrop for {} was dropped without closing::close() being called.",
                thread::current().name().unwrap_or("unnamed")
            );
            close()
        }
    }
}

pub fn closed() -> bool {
    CLOSED.load(Ordering::Relaxed)
}

pub async fn closed_fut() {
    // We only care that it's closed.
    let _ = CLOSER.1.recv_async().await;
}

pub fn close() {
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
    }
}

pub fn init(gui_sender: glib::Sender<GuiAction>) {
    Lazy::force(&CLOSER);

    GUI_CLOSER
        .set(gui_sender)
        .expect("closing::init() called twice");

    #[cfg(target_family = "unix")]
    spawn_thread("signals", || {
        let _cod = CloseOnDrop::default();

        for sig in signal_hook::consts::TERM_SIGNALS {
            // When terminated by a second term signal, exit with exit code 1.
            signal_hook::flag::register_conditional_shutdown(*sig, 1, CLOSED.clone())
                .expect("Error registering signal handlers.");
        }

        let mut sigs: Vec<c_int> = Vec::new();
        sigs.extend(signal_hook::consts::TERM_SIGNALS);
        let mut it = match iterator::SignalsInfo::<iterator::exfiltrator::SignalOnly>::new(sigs) {
            Ok(i) => i,
            Err(e) => {
                error!("Error registering signal handlers: {:?}", e);
                close();
                return;
            }
        };

        if let Some(s) = it.into_iter().next() {
            info!("Received signal {}, shutting down", s);
            close();
            it.handle().close();
            info!("closed {}", it.is_closed());
        }
    });
}
