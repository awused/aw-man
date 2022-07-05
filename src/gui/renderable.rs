use std::cell::RefCell;
use std::cmp::{max, min};
use std::convert::TryInto;
use std::mem::ManuallyDrop;
use std::rc::{Rc, Weak};
use std::time::{Duration, Instant};

use gdk4_x11::glib::clone::Downgrade;
use gtk::cairo::ffi::cairo_surface_get_reference_count;
use gtk::glib::SourceId;
use gtk::prelude::*;
use gtk::{cairo, glib};

use super::Gui;
use crate::com::{AnimatedImage, Bgra, DedupedVec, Res, ScaledImage};


#[derive(Debug)]
pub(super) struct SurfaceContainer {
    // Fields are dropped in FIFO order, ensuring bgra always outlives surface.
    pub(super) surface: cairo::ImageSurface,
    internal_scroll_region: Res,
    internal_scroll_position: (u32, u32),
    pub(super) bgra: Bgra,
    pub(super) original_res: Res,
}

impl From<&ScaledImage> for SurfaceContainer {
    fn from(sbgra: &ScaledImage) -> Self {
        Self::new(&sbgra.bgra, sbgra.original_res)
    }
}

// Repaging is not cheap so we do this in discrete chunks, the maximum monitor resolution supported
// is SCROLL_CHUNK_SIZE - TILE_SIZE.
// This is a pretty naive implementation that tries to balance smooth scrolling against frequent
// repaging, recognizing that images this large are very rare.
static TILE_SIZE: u32 = 16384;
static SCROLL_CHUNK_SIZE: u32 = 8192;

impl SurfaceContainer {
    // Not a From trait just to keep it from being misused by mistake.
    pub(super) fn from_unscaled(bgra: &Bgra) -> Self {
        Self::new(bgra, bgra.res)
    }

    fn new(bgra: &Bgra, original_res: Res) -> Self {
        static MAX: u32 = (i16::MAX - 1) as u32;
        // How much scrolling can/must be performed internally due to the limitations of cairo
        // surfaces.
        let scroll_x = if bgra.res.w <= MAX { 0 } else { bgra.res.w.saturating_sub(TILE_SIZE) };
        let scroll_y = if bgra.res.h <= MAX { 0 } else { bgra.res.h.saturating_sub(TILE_SIZE) };


        let scroll_region: Res = (scroll_x, scroll_y).into();

        let w = (bgra.res.w - scroll_region.w) as i32;
        let h = (bgra.res.h - scroll_region.h) as i32;

        if !scroll_region.is_zero() {
            debug!("Image too large for cairo: {:?}, max width/height: {MAX}", bgra.res);
        }

        // Use unsafe to create a cairo::ImageSurface which requires mutable access
        // to the underlying image data without needing to duplicate the entire image
        // in memory.
        let raw_ptr = bgra.as_ptr();
        let surface = unsafe {
            // ImageSurface can be used to mutate the underlying data.
            // This is safe because the image data is never mutated in this program.
            let mut_ptr = raw_ptr as *mut u8;
            cairo::ImageSurface::create_for_data_unsafe(
                mut_ptr,
                cairo::Format::ARgb32,
                w,
                h,
                bgra.stride.try_into().expect("Image too big"),
            )
            .expect("Invalid cairo surface state.")
        };

        Self {
            bgra: bgra.clone(),
            internal_scroll_region: scroll_region,
            internal_scroll_position: (0, 0),
            surface,
            original_res,
        }
    }

    fn update_surface_with_offset(&mut self, x: u32, y: u32) {
        let w = self.surface.width();
        let h = self.surface.height();

        assert!((x + w as u32) <= self.bgra.res.w);
        assert!((y + h as u32) <= self.bgra.res.h);

        let stride = self.surface.stride();
        // Point to the upper left corner of the sub-image
        let raw_ptr = self.bgra.as_offset_ptr(x, y);

        let surface = unsafe {
            // ImageSurface can be used to mutate the underlying data.
            // This is safe because the image data is never mutated in this program.
            let mut_ptr = raw_ptr as *mut u8;
            cairo::ImageSurface::create_for_data_unsafe(
                mut_ptr,
                cairo::Format::ARgb32,
                w,
                h,
                stride,
            )
            .expect("Invalid cairo surface state.")
        };

        unsafe {
            // Assert no other references to the old surface right before we destroy it.
            assert!(cairo_surface_get_reference_count(self.surface.to_raw_none()) == 1);
        }

        self.surface = surface;
        self.internal_scroll_position = (x, y);
    }

    // Performs internal scrolling for very large images, if necessary, and returns the remaining
    // amount that needs to be translated by cairo.
    // To simplify code - so it's resolution independent - we perform internal scrolling "first",
    // then apply cairo translations,
    pub(super) fn internal_scroll(&mut self, x: i32, y: i32) -> (i32, i32) {
        if self.internal_scroll_region.is_zero() {
            return (x, y);
        }

        let mut internal_x = min(self.internal_scroll_region.w, max(x.saturating_neg(), 0) as u32);
        let mut internal_y = min(self.internal_scroll_region.h, max(y.saturating_neg(), 0) as u32);

        if internal_x != self.internal_scroll_region.w {
            internal_x -= internal_x % SCROLL_CHUNK_SIZE;
        }
        if internal_y != self.internal_scroll_region.h {
            internal_y -= internal_y % SCROLL_CHUNK_SIZE;
        }

        if self.internal_scroll_position != (internal_x, internal_y) {
            let start = Instant::now();
            self.update_surface_with_offset(internal_x, internal_y);
            trace!(
                "Scrolling internally inside large surface to {internal_x}x{internal_y}: {:?}",
                start.elapsed()
            );
        }

        (x + internal_x as i32, y + internal_y as i32)
    }
}

impl Drop for SurfaceContainer {
    fn drop(&mut self) {
        unsafe {
            // We never, ever clone the surface, so this is just a sanity check to ensure that the
            // surface can't outlive the image data.
            assert!(cairo_surface_get_reference_count(self.surface.to_raw_none()) == 1);
        }
    }
}

#[derive(Debug)]
pub(super) enum AnimationStatus {
    Paused(Duration),
    Playing {
        target_time: Instant,
        timeout_id: ManuallyDrop<SourceId>,
    },
}

impl Drop for AnimationStatus {
    fn drop(&mut self) {
        match self {
            Self::Paused(_) => (),
            Self::Playing { timeout_id, .. } => unsafe { ManuallyDrop::take(timeout_id).remove() },
        }
    }
}

#[derive(Debug)]
pub(super) struct AnimationContainer {
    pub(super) animated: AnimatedImage,
    pub(super) surfaces: DedupedVec<SurfaceContainer>,
    pub(super) index: usize,
    pub(super) status: AnimationStatus,
}

impl AnimationContainer {
    pub fn new(a: &AnimatedImage, g: Rc<Gui>) -> Rc<RefCell<Self>> {
        let surfaces = a.frames().map(|f| SurfaceContainer::from_unscaled(&f.0));

        // We can safely assume this will be drawn as soon as possible.
        let target_time = Instant::now()
            .checked_add(a.frames()[0].1)
            .unwrap_or_else(|| Instant::now() + Duration::from_secs(1));

        Rc::new_cyclic(|weak| {
            let w = weak.clone();
            let timeout_id =
                ManuallyDrop::new(glib::timeout_add_local_once(a.frames()[0].1, move || {
                    Self::advance_animation(w, g)
                }));

            RefCell::new(Self {
                animated: a.clone(),
                surfaces,
                index: 0,
                status: AnimationStatus::Playing { target_time, timeout_id },
            })
        })
    }

    fn set_playing(rc: &Rc<RefCell<Self>>, g: &Rc<Gui>, play: Option<bool>) {
        let mut ab = rc.borrow_mut();

        match (play, &ab.status) {
            (Some(false) | None, AnimationStatus::Playing { target_time, .. }) => {
                let residual = target_time.saturating_duration_since(Instant::now());
                ab.status = AnimationStatus::Paused(residual);
            }
            (Some(true) | None, AnimationStatus::Paused(residual)) => {
                let g = g.clone();
                let w = rc.downgrade();
                let target_time = Instant::now()
                    .checked_add(*residual)
                    .unwrap_or_else(|| Instant::now() + Duration::from_secs(1));
                let timeout_id =
                    ManuallyDrop::new(glib::timeout_add_local_once(*residual, move || {
                        Self::advance_animation(w, g)
                    }));

                ab.status = AnimationStatus::Playing { target_time, timeout_id };
            }
            (Some(true), AnimationStatus::Playing { .. })
            | (Some(false), AnimationStatus::Paused(..)) => (),
        }
    }

    fn advance_animation(weak: Weak<RefCell<Self>>, g: Rc<Gui>) {
        let rc = weak.upgrade().expect("Impossible");
        let mut ab = rc.borrow_mut();
        let mut ac = &mut *ab;

        let (target_time, timeout_id) =
            if let AnimationStatus::Playing { target_time, timeout_id } = &mut ac.status {
                (target_time, &mut **timeout_id)
            } else {
                unreachable!()
            };

        while *target_time < Instant::now() {
            ac.index = (ac.index + 1) % ac.animated.frames().len();
            let mut dur = ac.animated.frames()[ac.index].1;
            if dur.is_zero() {
                dur = Duration::from_millis(100);
            }

            *target_time = target_time
                .checked_add(dur)
                .unwrap_or_else(|| Instant::now() + Duration::from_secs(1));
        }

        g.canvas.queue_draw();

        *timeout_id = glib::timeout_add_local_once(
            target_time.saturating_duration_since(Instant::now()),
            move || Self::advance_animation(weak, g),
        );
    }
}

// TODO -- preload video https://gitlab.gnome.org/GNOME/gtk/-/issues/4062
// #[derive(Debug)]
// struct VideoContainer {
//     // Fields are dropped in FIFO order, ensuring the data will always outlive the references.
//     video: gtk::Video,
//     media_file: gtk::MediaFile,
//     input_stream: gio::MemoryInputStream,
//     bytes: glib::Bytes,
//     data: VideoData,
//
//     detached: Cell<bool>,
// }
//
//
// impl VideoContainer {
//     fn new(vd: &VideoData, parent: &gtk::Overlay) -> Self {
//         let data = vd.clone();
//
//         let bytes;
//         let r = data.as_ref() as *const [u8];
//         unsafe {
//             // r is a pointer to a Pin<Arc<Vec<u8>>> which is guaranteed not to move for the
//             // lifetime of the Arc. We hold an immutable reference to the Arc, so it
//             // cannot be destroyed, so this lifetime upcast is safe for this
//             // application. This avoids copying the video data each time it is
//             // displayed.
//             let r = &*r;
//             bytes = glib::Bytes::from_static(r);
//         }
//         let input_stream = gio::MemoryInputStream::from_bytes(&bytes);
//         let media_file = gtk::MediaFile::for_input_stream(&input_stream);
//         let video = gtk::Video::new();
//
//         video.set_halign(Align::Center);
//         video.set_valign(Align::Center);
//
//         video.set_hexpand(false);
//         video.set_vexpand(false);
//
//         video.set_autoplay(true);
//         video.set_loop(true);
//
//         video.set_media_stream(Some(&media_file));
//
//         parent.add_overlay(&video);
//
//         Self {
//             video,
//             media_file,
//             input_stream,
//             bytes,
//             data,
//
//             detached: Cell::new(false),
//         }
//     }
//
//     fn detach(self, parent: &gtk::Overlay) {
//         if let Some(p) = &self.video.parent() {
//             let p = p
//                 .dynamic_cast_ref::<gtk::Overlay>()
//                 .expect("Video attached to non-overlay parent.");
//             p.remove_overlay(&self.video);
//         }
//
//         self.detached.set(true);
//         drop(self);
//     }
// }
//
// impl Drop for VideoContainer {
//     fn drop(&mut self) {
//         if !self.detached.get() {
//             error!("VideoContainer dropped without detaching from parent.");
//             closing::close();
//         }
//     }
// }

// enum Offscreen {
//     Rendered(SurfaceContainer),
//     // No more room to scroll
//     Nothing,
// }

// #[derive(Debug)]
// enum VisibleContent {
//     Single(Displayed),
//     Continuous {
//         total_visible_height: u32,
//     // TODO -- idle_unload these
//         prev: Option<Displayed>,
//         visible: Vec<Displayed>,
//         next: Option<Displayed>,
//     },
// }

// Like Displayable but with any additional metadata about its current state.
#[derive(Debug)]
pub(super) enum Displayed {
    Image(SurfaceContainer),
    Animation(Rc<RefCell<AnimationContainer>>),
    Video(gtk::Video),
    Error(gtk::Label),
    // Multiple displayed images for continuous scrolling
    // Continuous {
    //   // For now, a single one is enough. This might need to be expanded to cover at least a
    //   single scroll event.
    //   prev: Option<SurfaceContainer>,
    //   visible: Vec<SurfaceContainer>,
    //   next: Option<SurfaceContainer>,
    // }
    // TODO -- everything, not just SurfaceContainers
    Pending(Res),
    Nothing,
}

impl Displayed {
    pub(super) fn set_playing(&mut self, g: &Rc<Gui>, play: Option<bool>) {
        // TODO -- set_playing(None) for multiple animations/videos at once should set them to the
        // same value.
        match self {
            Self::Animation(a) => AnimationContainer::set_playing(a, g, play),
            Self::Video(v) => {
                let ms = v.media_stream().unwrap();
                match (play, ms.is_playing()) {
                    (Some(false) | None, true) => ms.set_playing(false),
                    (Some(true) | None, false) => ms.set_playing(true),
                    (Some(true), true) | (Some(false), false) => (),
                }
            }
            _ => {}
        }
    }
}
