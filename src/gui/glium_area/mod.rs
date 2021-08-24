mod imp;
mod renderable;

use std::ptr;

use gtk::glib::ObjectType;
use gtk::prelude::{GLAreaExt, WidgetExt};
use gtk::subclass::prelude::ObjectSubclassExt;
use gtk::{gdk, glib};

use self::imp::GliumGLArea;

glib::wrapper! {
    pub struct GliumArea(ObjectSubclass<imp::GliumGLArea>)
        @extends gtk::GLArea, gtk::Widget;
}

impl Default for GliumArea {
    fn default() -> Self {
        Self::new()
    }
}

impl GliumArea {
    pub fn new() -> Self {
        let s: Self = glib::Object::new(&[]).expect("Failed to create GliumArea");

        s.connect_resize(|s, w, h| {
            println!("context {:?}", s.context().as_ref().unwrap().as_ptr());
            println!("resize {w}, {h}");

            s.inner().invalidate();
        });

        s
    }

    pub fn inner(&self) -> &GliumGLArea {
        GliumGLArea::from_instance(self)
    }
}

unsafe impl glium::backend::Backend for GliumArea {
    fn swap_buffers(&self) -> Result<(), glium::SwapBuffersError> {
        // We're supposed to draw (and hence swap buffers) only inside the `render()` vfunc or
        // signal, which means that GLArea will handle buffer swaps for us.
        Ok(())
    }

    unsafe fn get_proc_address(&self, symbol: &str) -> *const std::ffi::c_void {
        epoxy::get_proc_addr(symbol)
    }

    fn get_framebuffer_dimensions(&self) -> (u32, u32) {
        let scale = self.scale_factor();
        let width = self.width();
        let height = self.height();
        ((width * scale) as u32, (height * scale) as u32)
    }

    fn is_current(&self) -> bool {
        match self.context() {
            Some(context) => gdk::GLContext::current() == Some(context),
            None => false,
        }
    }

    unsafe fn make_current(&self) {
        GLAreaExt::make_current(self);
    }
}

pub(super) fn init() {
    // Load GL pointers from epoxy (GL context management library used by GTK).
    #[cfg(target_os = "macos")]
    let library = unsafe { libloading::os::unix::Library::new("libepoxy.0.dylib") }.unwrap();
    #[cfg(all(unix, not(target_os = "macos")))]
    let library = unsafe { libloading::os::unix::Library::new("libepoxy.so.0") }.unwrap();
    #[cfg(windows)]
    let library = libloading::os::windows::Library::open_already_loaded("libepoxy-0.dll").unwrap();

    // epoxy needs to be loaded now, but the gl library can wait until rendering is started.
    epoxy::load_with(|name| {
        unsafe { library.get::<_>(name.as_bytes()) }
            .map(|symbol| *symbol)
            .unwrap_or(ptr::null())
    });
}
