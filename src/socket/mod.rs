use std::fs::remove_file;
use std::path::{Path, PathBuf};
use std::{io, process, thread};

use gtk::glib::Sender;
use once_cell::sync::OnceCell;
use serde_json::Value;
#[cfg(target_family = "unix")]
use tokio::net::{UnixListener, UnixStream};
use tokio::select;
use tokio::sync::oneshot;

use crate::com::GuiAction;
use crate::{closing, config, spawn_thread};

pub static SOCKET_PATH: OnceCell<PathBuf> = OnceCell::new();

struct RemoveOnDrop {}

impl Drop for RemoveOnDrop {
    fn drop(&mut self) {
        if let Some(p) = SOCKET_PATH.get() {
            drop(remove_file(p));
        }
    }
}

pub(super) fn init(gui_sender: &Sender<GuiAction>) -> Option<thread::JoinHandle<()>> {
    #[cfg(target_family = "unix")]
    if let Some(d) = &config::CONFIG.socket_dir {
        SOCKET_PATH
            .set(PathBuf::from(d).join(format!("aw-man{}.sock", process::id())))
            .expect("Failed to set socket path");

        let sock = SOCKET_PATH.get().expect("Impossible");
        let gui_sender = gui_sender.clone();

        let h = spawn_thread("socket", move || listen(sock, gui_sender));
        Some(h)
    } else {
        None
    }

    #[cfg(target_family = "windows")]
    None
}

async fn handle_command(cmd: String, gui_sender: &Sender<GuiAction>) -> Value {
    let (s, r) = oneshot::channel();
    let ga = GuiAction::Action(cmd, s);

    if let Err(e) = gui_sender.send(ga) {
        let e = format!("Error sending socket commend to Gui: {:?}", e);
        error!("{}", e);
        return Value::String(e);
    };

    // An error will most likely mean the value was dropped.
    match r.await {
        Ok(v) => v,
        Err(_) => Value::String("done".to_string()),
    }
}

#[cfg(target_family = "unix")]
async fn handle_stream(stream: UnixStream, gui_sender: Sender<GuiAction>) {
    loop {
        select! {
           r = stream.readable() => {
               match r  {
                   Ok(_) => {}
                   Err(e) => {
                       error!("Socket stream error {:?}", e);
                       return;
                   }
               }
           }
           _ = closing::closed_fut() => return,
        }

        // Any realistic command (for now) will be under 1KB.
        // This will most likely change in the future.
        let mut msg = vec![0; 1024];
        match stream.try_read(&mut msg) {
            Ok(n) => {
                msg.truncate(n);
            }
            Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => {
                continue;
            }
            Err(e) => {
                error!("Socket stream error {:?}", e);
                return;
            }
        }

        // No message read, we're done.
        if msg.is_empty() {
            return;
        }

        let resp = match std::str::from_utf8(&msg) {
            Ok(cmd) => {
                let cmd = cmd.trim();
                handle_command(cmd.to_string(), &gui_sender).await
            }
            Err(e) => {
                let e = format!("Unable to parse command {:?}", e);
                error!("{}", e);
                Value::String(e)
            }
        };

        let resp = resp.to_string();
        let byte_resp = resp.as_bytes();
        let mut i = 0;

        loop {
            select! {
               r = stream.writable() => {
                   match r  {
                       Ok(_) => {}
                       Err(e) => {
                           error!("Socket stream error {:?}", e);
                           return;
                       }
                   }
               }
               _ = closing::closed_fut() => return,
            }

            match stream.try_write(&byte_resp[i..]) {
                Ok(n) => {
                    i += n;
                    if i >= byte_resp.len() {
                        break;
                    }
                }
                Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => {
                    continue;
                }
                Err(e) => {
                    error!("Socket stream error {:?}", e);
                    return;
                }
            }
        }
    }
}

#[cfg(target_family = "unix")]
#[tokio::main(flavor = "current_thread")]
async fn listen(sock: &Path, gui_sender: Sender<GuiAction>) {
    let listener = UnixListener::bind(&sock);
    let listener = match listener {
        Ok(l) => l,
        Err(e) => {
            error!("Failed to open socket {:?}: {:?}", sock, e);
            closing::close();
            drop(remove_file(sock));
            return;
        }
    };
    info!("Listening on {:?}", sock);
    let _rmdrop = RemoveOnDrop {};

    loop {
        select! {
           conn = listener.accept() => {
               match conn  {
                   Ok((stream, _addr)) => {
                       let gui_sender = gui_sender.clone();
                       tokio::spawn(async {
                           handle_stream(stream, gui_sender).await
                       });
                   }
                   Err(e) => {
                       error!("Socket listener error {:?}", e);
                   }
               }
           }
           _ = closing::closed_fut() => break,
        }
    }
    drop(listener);
}
