// This file contains the structures references by both the gui and manager side of the
// application.

use std::collections::hash_map::Entry;
use std::collections::HashMap;
use std::convert::TryInto;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;
use std::{fmt, thread};

use derive_more::{Deref, DerefMut, From};
use image::{DynamicImage, RgbaImage};
use tokio::sync::oneshot;

#[derive(Deref)]
struct DataBuf(Vec<u8>);

impl Drop for DataBuf {
    fn drop(&mut self) {
        trace!(
            "Cleaned up {:.2}MB in {}",
            (self.0.len() as f64) / 1_048_576.0,
            thread::current().name().unwrap_or("unknown")
        )
    }
}

#[derive(Clone)]
pub struct Bgra {
    // Explicitly pinning is likely to be unnecessary, but not harmful.
    buf: Pin<Arc<DataBuf>>,
    pub res: Res,
    pub stride: u32,
}

impl PartialEq for Bgra {
    fn eq(&self, other: &Self) -> bool {
        self.buf.as_ptr() == other.buf.as_ptr()
    }
}
impl Eq for Bgra {}

impl fmt::Debug for Bgra {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[Bgra: {:?}]", self.res)
    }
}

impl From<DynamicImage> for Bgra {
    fn from(img: DynamicImage) -> Self {
        let mut img = img.into_rgba8();
        img.chunks_exact_mut(4).for_each(|c| c.swap(0, 2));
        Self::from_bgra_buffer(img)
    }
}

impl Bgra {
    pub fn as_ptr(&self) -> *const u8 {
        self.buf.as_ptr()
    }

    // Get a pointer to an indiviual pixel, useful for rendering extremely large images with
    // offsets.
    pub fn as_offset_ptr(&self, x: u32, y: u32) -> *const u8 {
        &self.buf[y as usize * self.stride as usize + x as usize * 4] as *const u8
    }

    pub fn clone_image_buffer(&self) -> RgbaImage {
        let container = (*self.buf).clone();
        RgbaImage::from_vec(self.res.w, self.res.h, container)
            .expect("Conversion back to image buffer cannot fail")
    }

    pub fn from_bgra_buffer(img: RgbaImage) -> Self {
        let res = Res::from(img.dimensions());
        let stride = img
            .sample_layout()
            .height_stride
            .try_into()
            .expect("Image corrupted or too large");
        Self {
            buf: Arc::pin(DataBuf(img.into_raw())),
            res,
            stride,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScaledImage {
    pub bgra: Bgra,
    pub original_res: Res,
}

#[derive(Deref, From)]
pub struct Frames(Vec<(Bgra, Duration, u64)>);

impl Drop for Frames {
    fn drop(&mut self) {
        let count = self.0.len();
        self.0.drain(..);
        trace!("Cleaned up {} frames", count)
    }
}

// TODO -- not entirely happy with this, it wastes too much memory and still isn't nearly as
// cpu-efficient as it should be on rendering.
// Will have to explore better options in the future, but for now, it works and is generic enough.
// Even in the future this can be kept as a fallback for weird formats.
#[derive(Clone)]
pub struct AnimatedImage {
    frames: Arc<Frames>,
    _dur: Duration,
}


impl PartialEq for AnimatedImage {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.frames, &other.frames)
    }
}
impl Eq for AnimatedImage {}

impl fmt::Debug for AnimatedImage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // There must always be at least one frame
        write!(f, "AnimatedImage {} * {:?}", self.frames.len(), self.frames[0].0.res)
    }
}

impl AnimatedImage {
    pub fn new(mut frames: Frames) -> Self {
        assert!(!frames.is_empty());

        let dur = frames.iter().fold(Duration::ZERO, |dur, frame| dur.saturating_add(frame.1));

        let mut hashed_frames: HashMap<u64, &Bgra> = HashMap::new();
        let mut deduped_frames = 0;
        let mut deduped_bytes = 0;

        for f in frames.0.iter_mut() {
            match hashed_frames.entry(f.2) {
                Entry::Occupied(e) => {
                    let dupe = std::mem::replace(&mut f.0, (*e.get()).clone());
                    deduped_bytes += dupe.buf.len();
                    deduped_frames += 1;
                }
                Entry::Vacant(e) => {
                    e.insert(&f.0);
                }
            }
        }

        if deduped_frames != 0 {
            debug!(
                "Deduped {} frames saving {:.2}MB",
                deduped_frames,
                deduped_bytes as f64 / 1_048_576.0
            );
        }

        Self { frames: Arc::from(frames), _dur: dur }
    }

    pub const fn frames(&self) -> &Arc<Frames> {
        &self.frames
    }
}

// TODO -- preload video https://gitlab.gnome.org/GNOME/gtk/-/issues/4062
// #[derive(Clone)]
// pub struct VideoData {
//     buf: Pin<Arc<DataBuf>>,
// }
//
// impl From<Vec<u8>> for VideoData {
//     fn from(buf: Vec<u8>) -> Self {
//         Self {
//             buf: Arc::pin(DataBuf(buf)),
//         }
//     }
// }
//
// impl fmt::Debug for VideoData {
//     fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
//         write!(
//             f,
//             "VideoData {:.2}MB",
//             (self.buf.len() as f64) / 1_048_576.0
//         )
//     }
// }
//
// impl VideoData {
//     pub fn as_ref(&self) -> &[u8] {
//         &self.buf
//     }
// }
//

#[derive(Debug, Eq, Clone)]
pub enum Displayable {
    Image(ScaledImage),
    Animation(AnimatedImage),
    Video(PathBuf),
    Error(String),
    Nothing, // Generally for loading.
}

impl Default for Displayable {
    fn default() -> Self {
        Self::Nothing
    }
}


impl std::cmp::PartialEq for Displayable {
    fn eq(&self, other: &Self) -> bool {
        use Displayable::*;
        match (self, other) {
            (Image(arc), Image(oarc)) => arc.bgra == oarc.bgra,
            (Error(s), Error(os)) => s == os,
            (Animation(sa), Animation(oa)) => sa == oa,
            (Video(sv), Video(ov)) => sv == ov,
            (Nothing, Nothing) => true,
            (..) => false,
        }
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Modes {
    pub manga: bool,
    pub upscaling: bool,
    pub fit: Fit,
}

impl Modes {
    pub fn gui_str(self) -> String {
        let mut out = String::default();
        if self.upscaling {
            out.push('U')
        }
        if self.manga {
            out.push('M');
        }
        out
    }
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum Direction {
    Absolute,
    Forwards,
    Backwards,
}

pub type CommandResponder = oneshot::Sender<serde_json::Value>;

pub type MAWithResponse = (ManagerAction, Option<CommandResponder>);

#[derive(Debug, PartialEq, Eq)]
pub enum ManagerAction {
    Resolution(Res),
    MovePages(Direction, usize),
    NextArchive,
    PreviousArchive,
    Status,
    ListPages,
    Execute(String),
    ToggleUpscaling,
    ToggleManga,
    FitStrategy(Fit),
}

#[derive(Default, PartialEq, Eq, Copy, Clone)]
pub struct Res {
    pub w: u32,
    pub h: u32,
}

impl fmt::Debug for Res {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}x{}", self.w, self.h)
    }
}

// Just allow panics because this should only ever be used to convert to/from formats that use
// signed but never negative widths/heights.
#[allow(clippy::fallible_impl_from)]
impl From<(i32, i32)> for Res {
    fn from(wh: (i32, i32)) -> Self {
        if wh.0 < 0 || wh.1 < 0 {
            panic!("Can't have negative width or height");
        }

        Self { w: wh.0 as u32, h: wh.1 as u32 }
    }
}

impl From<(u32, u32)> for Res {
    fn from(wh: (u32, u32)) -> Self {
        Self { w: wh.0, h: wh.1 }
    }
}

impl From<DynamicImage> for Res {
    fn from(di: DynamicImage) -> Self {
        (di.width(), di.height()).into()
    }
}

impl Res {
    pub const fn is_zero_area(self) -> bool {
        self.w == 0 || self.h == 0
    }

    pub const fn is_zero(self) -> bool {
        self.w == 0 && self.h == 0
    }

    pub fn fit_inside(self, t: TargetRes) -> Self {
        let (w, h) = (self.w as f64, self.h as f64);

        let scale = match t.fit {
            Fit::Container => {
                let (tw, th) = (t.res.w as f64, t.res.h as f64);
                f64::min(tw / w, th / h)
            }
            Fit::Height => t.res.h as f64 / h,
            Fit::Width => t.res.w as f64 / w,
            Fit::FullSize => return self,
        };

        if scale <= 0.0 || scale >= 1.0 || !scale.is_finite() {
            return self;
        }

        Self {
            w: (w * scale).round() as u32,
            h: (h * scale).round() as u32,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fit {
    Container,
    Height,
    Width,
    FullSize,
}

impl Default for Fit {
    fn default() -> Self {
        Self::Container
    }
}


#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct TargetRes {
    pub res: Res,
    pub fit: Fit,
}

impl From<(i32, i32, Fit)> for TargetRes {
    fn from((w, h, fit): (i32, i32, Fit)) -> Self {
        Self { res: (w, h).into(), fit }
    }
}

#[derive(Debug, PartialEq, Eq, Copy, Clone)]
pub struct WorkParams {
    pub park_before_scale: bool,
    pub jump_downscaling_queue: bool,
    pub extract_early: bool,
    pub target_res: TargetRes,
}

// Represents the current displayable and its metadata.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct GuiState {
    pub displayable: Displayable,
    pub page_num: usize,
    pub page_name: String,
    pub archive_len: usize,
    pub archive_name: String,
    pub modes: Modes,
    pub target_res: TargetRes,
}

#[derive(Debug)]
pub enum GuiAction {
    State(GuiState),
    Action(String, CommandResponder),
    Quit,
}

#[derive(Deref, DerefMut, From)]
pub struct DebugIgnore<T>(pub T);

impl<T> fmt::Debug for DebugIgnore<T> {
    fn fmt(&self, _f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Result::Ok(())
    }
}

impl<T: Default> Default for DebugIgnore<T> {
    fn default() -> Self {
        Self(Default::default())
    }
}
