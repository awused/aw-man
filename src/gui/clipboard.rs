use std::path::Path;
use std::sync::Arc;

use gtk::subclass::prelude::ObjectSubclassIsExt;
use gtk::{gdk, glib};

pub const SPECIAL: &str = "x-special/aw-fm-copied-files";
pub const SPECIAL_MATE: &str = "x-special/mate-copied-files";
pub const SPECIAL_GNOME: &str = "x-special/gnome-copied-files";
pub const URIS: &str = "text/uri-list";

glib::wrapper! {
    pub struct SelectionProvider(ObjectSubclass<imp::ClipboardProvider>)
        @extends gdk::ContentProvider;
}


impl SelectionProvider {
    // It's fine if the selection is empty, we still want to overwrite the contents of the
    // clipboard for safety.
    pub fn new(file: Option<Arc<Path>>) -> Self {
        let s: Self = glib::Object::new();

        // TODO -- do we really need to support multiple things or is this just easier to
        // maintain from aw-fm?
        s.imp().entries.set(file.into_iter().collect()).unwrap();

        s
    }
}

mod imp {
    use std::cell::OnceCell;
    use std::ffi::OsString;
    use std::future::Future;
    #[cfg(unix)]
    use std::os::unix::ffi::OsStringExt;
    #[cfg(unix)]
    use std::os::unix::prelude::OsStrExt;
    use std::path::Path;
    use std::pin::Pin;
    use std::rc::Rc;
    use std::sync::Arc;

    use gtk::glib::GString;
    use gtk::glib::value::ToValue;
    use gtk::prelude::{FileExt, OutputStreamExt};
    use gtk::subclass::prelude::*;
    use gtk::{gdk, gio, glib};

    use super::{SPECIAL, SPECIAL_GNOME, SPECIAL_MATE, URIS};


    // TODO -- application/vnd.portal.filetransfer, if it ever comes up
    const UTF8: &str = "text/plain;charset=utf-8";
    const PLAIN: &str = "text/plain";
    const UTF8_STRING: &str = "UTF8_STRING";
    const COMPOUND_TEXT: &str = "COMPOUND_TEXT";
    const TEXT: &str = "TEXT";
    const STRING: &str = "STRING";

    #[derive(Default)]
    pub struct ClipboardProvider {
        pub entries: OnceCell<Rc<[Arc<Path>]>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for ClipboardProvider {
        type ParentType = gdk::ContentProvider;
        type Type = super::SelectionProvider;

        const NAME: &'static str = "ClipboardProvider";
    }

    impl ObjectImpl for ClipboardProvider {}

    impl ContentProviderImpl for ClipboardProvider {
        fn formats(&self) -> gdk::ContentFormats {
            gdk::ContentFormatsBuilder::new()
                .add_mime_type(SPECIAL)
                .add_mime_type(SPECIAL_MATE)
                .add_mime_type(SPECIAL_GNOME)
                .add_mime_type(URIS)
                .add_mime_type(UTF8)
                .add_mime_type(PLAIN)
                .add_mime_type(UTF8_STRING)
                .add_mime_type(COMPOUND_TEXT)
                .add_mime_type(TEXT)
                .add_mime_type(STRING)
                .build()
        }

        fn value(&self, gtype: glib::Type) -> Result<glib::Value, glib::Error> {
            if gtype == glib::Type::STRING {
                let mut out = OsString::new();
                self.entries.get().unwrap().iter().for_each(|e| out.push(e.as_os_str()));

                #[cfg(unix)]
                let out = out.into_vec();
                #[cfg(not(unix))]
                let out = match out.into_string() {
                    Ok(s) => s.into_bytes(),
                    Err(e) => {
                        error!("Failed to convert set of paths to valid utf-8: {e:?}");
                        return Ok(GString::new().to_value());
                    }
                };

                match GString::from_utf8(out) {
                    Ok(s) => Ok(s.to_value()),
                    Err(e) => {
                        error!("Failed to convert set of paths to valid utf-8: {e:?}");
                        Ok(GString::new().to_value())
                    }
                }
            } else {
                panic!("Got unknown glib type {gtype:?} in ContentProviderImpl::value()");
            }
        }

        fn write_mime_type_future(
            &self,
            mime_type: &str,
            stream: &gio::OutputStream,
            priority: glib::Priority,
        ) -> Pin<Box<dyn Future<Output = Result<(), glib::Error>> + 'static>> {
            let stream = stream.clone();
            let mime_type = mime_type.to_string();
            let entries = self.entries.get().unwrap().clone();

            Box::pin(async move {
                match &*mime_type {
                    SPECIAL | SPECIAL_MATE | SPECIAL_GNOME => {
                        write_bytes(&stream, priority, b"copy").await?;

                        write_bytes(&stream, priority, b"\n").await?;

                        Self::write_uris(&stream, priority, &entries).await
                    }
                    URIS => Self::write_uris(&stream, priority, &entries).await,
                    UTF8 | UTF8_STRING | COMPOUND_TEXT | TEXT | STRING => {
                        Self::write_paths(&stream, priority, &entries).await
                    }
                    PLAIN => Self::write_ascii_paths(&stream, priority, &entries).await,
                    _ => {
                        Err(glib::Error::new(gio::IOErrorEnum::InvalidData, "Unhandled mime type"))
                    }
                }
            })
        }
    }

    async fn write_bytes(
        stream: &gio::OutputStream,
        priority: glib::Priority,
        mut bytes: &[u8],
    ) -> Result<(), glib::Error> {
        while !bytes.is_empty() {
            let n = stream.write_bytes_future(&glib::Bytes::from(&bytes), priority).await?;
            if n <= 0 {
                trace!("Failed to finish writing clipboard contents: {} unsent", bytes.len());
                break;
            }
            bytes = &bytes[(n as usize)..];
        }
        Ok(())
    }

    impl ClipboardProvider {
        async fn write_uris(
            stream: &gio::OutputStream,
            priority: glib::Priority,
            entries: &[Arc<Path>],
        ) -> Result<(), glib::Error> {
            let mut output = String::new();
            let mut iter = entries.iter();
            if let Some(first) = iter.next() {
                output += &gio::File::for_path(first).uri();

                for f in iter {
                    output.push('\n');
                    output += &gio::File::for_path(f).uri();
                }
            }

            write_bytes(stream, priority, &output.into_bytes()).await
        }

        async fn write_paths(
            stream: &gio::OutputStream,
            priority: glib::Priority,
            entries: &[Arc<Path>],
        ) -> Result<(), glib::Error> {
            let mut output = OsString::new();
            let mut iter = entries.iter();
            if let Some(first) = iter.next() {
                output.push(first.as_os_str());

                for f in iter {
                    output.push("\n");
                    output.push(f.as_os_str());
                }
            }

            #[cfg(unix)]
            {
                write_bytes(stream, priority, output.as_bytes()).await
            }
            #[cfg(not(unix))]
            {
                write_bytes(stream, priority, output.into_string().unwrap_or_default().as_bytes())
                    .await
            }
        }

        // Doesn't match C-style escape sequences, but nothing should really use this
        async fn write_ascii_paths(
            stream: &gio::OutputStream,
            priority: glib::Priority,
            entries: &[Arc<Path>],
        ) -> Result<(), glib::Error> {
            let mut output = String::new();
            let mut iter = entries.iter();
            if let Some(first) = iter.next() {
                output.extend(first.to_string_lossy().escape_default());

                for f in iter {
                    output.push('\n');
                    output.extend(f.to_string_lossy().escape_default());
                }
            }

            write_bytes(stream, priority, &output.into_bytes()).await
        }
    }
}
