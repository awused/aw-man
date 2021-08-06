use std::fs::remove_file;
use std::path::PathBuf;
use std::{io, process, thread};

use gtk::glib::Sender;
use serde_json::Value;
use tokio::net::{UnixListener, UnixStream};
use tokio::select;
use tokio::sync::oneshot;

use crate::com::GuiAction;
use crate::{closing, config, spawn_thread};

struct RemoveOnDrop {
    p: PathBuf,
}

impl Drop for RemoveOnDrop {
    fn drop(&mut self) {
        drop(remove_file(&self.p));
    }
}

pub(super) fn init(gui_sender: &Sender<GuiAction>) -> Option<thread::JoinHandle<()>> {
    if let Some(d) = &config::CONFIG.socket_dir {
        let sock = PathBuf::from(d).join(format!("aw-man{}.sock", process::id()));
        let gui_sender = gui_sender.clone();

        let h = spawn_thread("socket", move || listen(sock, gui_sender));
        Some(h)
    } else {
        None
    }
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

        let resp;
        match std::str::from_utf8(&msg) {
            Ok(cmd) => {
                let cmd = cmd.trim();
                resp = handle_command(cmd.to_string(), &gui_sender).await;
            }
            Err(e) => {
                let e = format!("Unable to parse command {:?}", e);
                error!("{}", e);
                resp = Value::String(e);
            }
        }

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

            match stream.try_write(resp.to_string().as_bytes()) {
                Ok(_) => {
                    // TODO -- larger responses may need multiple calls.
                    break;
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

#[tokio::main(flavor = "current_thread")]
async fn listen(sock: PathBuf, gui_sender: Sender<GuiAction>) {
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
    let _rmdrop = RemoveOnDrop { p: sock };

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
