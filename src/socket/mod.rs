use std::fs::remove_file;
use std::path::PathBuf;
use std::{process, thread};

use gtk::glib::Sender;
use once_cell::sync::OnceCell;
use serde_json::Value;
use tokio::sync::oneshot;

use crate::com::GuiAction;
use crate::{config, spawn_thread};

pub static SOCKET_PATH: OnceCell<PathBuf> = OnceCell::new();

struct RemoveOnDrop {}

impl Drop for RemoveOnDrop {
    fn drop(&mut self) {
        if let Some(p) = SOCKET_PATH.get() {
            drop(remove_file(p));
        }
    }
}

pub fn init(gui_sender: &Sender<GuiAction>) -> Option<thread::JoinHandle<()>> {
    if let Some(d) = &config::CONFIG.socket_dir {
        SOCKET_PATH
            .set(PathBuf::from(d).join(format!("aw-man{}.sock", process::id())))
            .expect("Failed to set socket path");

        let sock = SOCKET_PATH.get().unwrap();
        let gui_sender = gui_sender.clone();

        Some(spawn_thread("socket", move || imp::listen(sock, gui_sender)))
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

#[cfg(unix)]
mod imp {
    use std::fs::remove_file;
    use std::path::Path;

    use gtk::glib::Sender;
    use serde_json::Value;
    use tokio::net::{UnixListener, UnixStream};
    use tokio::select;

    use crate::closing;
    use crate::com::GuiAction;
    use crate::socket::{handle_command, RemoveOnDrop};

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
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
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
                    Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
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
    pub(super) async fn listen(sock: &Path, gui_sender: Sender<GuiAction>) {
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
                           break;
                       }
                   }
               }
               _ = closing::closed_fut() => break,
            }
        }
        // Explicitly drop so it's closed before RemoveOnDrop
        drop(listener);
    }
}

#[cfg(windows)]
mod imp {
    use std::fs::remove_file;
    use std::io::{ErrorKind, Read, Write};
    // use std::os::windows::io::AsRawSocket;
    use std::path::Path;
    use std::time::Duration;

    use futures_executor::block_on;
    use gtk::glib::Sender;
    use serde_json::Value;
    use uds_windows::{UnixListener, UnixStream};

    use crate::com::GuiAction;
    use crate::socket::{handle_command, RemoveOnDrop};
    use crate::{closing, spawn_thread};

    fn handle_stream(mut stream: UnixStream, gui_sender: Sender<GuiAction>) {
        if let Err(e) = stream.set_read_timeout(Some(Duration::from_secs(1))) {
            error!("Failed to set socket read timeout {e}");
            return;
        }
        if let Err(e) = stream.set_write_timeout(Some(Duration::from_secs(1))) {
            error!("Failed to set socket write timeout {e}");
            return;
        }

        loop {
            if closing::closed() {
                return;
            }

            let mut msg = vec![0; 1024];

            match stream.read(&mut msg) {
                Ok(n) => {
                    msg.truncate(n);
                }
                Err(e) => match e.kind() {
                    ErrorKind::WouldBlock | ErrorKind::TimedOut => {
                        continue;
                    }
                    _ => {
                        error!("Socket stream error {:?}", e);
                        return;
                    }
                },
            }

            // No message read, we're done.
            if msg.is_empty() {
                return;
            }

            let resp = match std::str::from_utf8(&msg) {
                Ok(cmd) => {
                    let cmd = cmd.trim();
                    block_on(handle_command(cmd.to_string(), &gui_sender))
                }
                Err(e) => {
                    let e = format!("Unable to parse command {:?}", e);
                    error!("{}", e);
                    Value::String(e)
                }
            };


            let resp = resp.to_string() + "\n";
            let byte_resp = resp.as_bytes();
            let mut i = 0;

            loop {
                match stream.write(&byte_resp[i..]) {
                    Ok(n) => {
                        i += n;
                        if i >= byte_resp.len() {
                            break;
                        }
                    }
                    Err(ref e) if e.kind() == ErrorKind::WouldBlock => {
                        if closing::closed() {
                            return;
                        }
                        continue;
                    }
                    Err(e) => {
                        if !closing::closed() {
                            error!("Socket stream error {:?}", e);
                        }
                        return;
                    }
                }
            }
        }
    }

    pub(super) fn listen(sock: &Path, gui_sender: Sender<GuiAction>) {
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

        listener.set_nonblocking(true).unwrap();

        // Equivalent code works on Linux but it doesn't seem to work on Windows.
        // unsafe {
        //     let sock = listener.as_raw_socket();
        //     let t = timeval { tv_sec: 1, tv_usec: 0 };
        //     assert_eq!(
        //         setsockopt(
        //             SOCKET(sock as usize),
        //             SOL_SOCKET as i32,
        //             SO_RCVTIMEO as i32,
        //             PCSTR(std::ptr::addr_of!(t) as _),
        //             std::mem::size_of::<timeval>() as i32,
        //         ),
        //         0
        //     );
        // }

        let mut count = 0;

        for conn in listener.incoming() {
            match conn {
                Ok(stream) => {
                    let gui_sender = gui_sender.clone();
                    spawn_thread(&format!("socket-stream-{}", count), move || {
                        handle_stream(stream, gui_sender)
                    });
                    count += 1;
                }
                Err(e) => {
                    if closing::closed() {
                        break;
                    }

                    if e.kind() != ErrorKind::WouldBlock {
                        error!("Socket listener error {e}");
                        break;
                    }

                    // Non-blocking + 1s sleep is not good, but it works at least until I can
                    // figure out what's going on with timeouts.
                    std::thread::sleep(Duration::from_secs(1));
                }
            }
        }
        // Explicitly drop so it's closed before RemoveOnDrop
        drop(listener);
    }
}
