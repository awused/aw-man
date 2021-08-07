// This file contains the structures references by both the gui and manager side of the
// application.

use std::convert::TryInto;
use std::fmt;
use std::pin::Pin;
use std::sync::Arc;

use derive_more::Deref;
use image::{DynamicImage, ImageBuffer};
use tokio::sync::oneshot;

#[derive(Deref)]
struct ImgBuf(Vec<u8>);

impl Drop for ImgBuf {
    fn drop(&mut self) {
        trace!("Cleaned up {:.2}MB", (self.0.len() as f64) / 1_048_576.0)
    }
}

#[derive(Clone)]
pub struct Bgra {
    // Explicitly pinning is likely to be unnecessary, but not harmful.
    buf: Pin<Arc<ImgBuf>>,
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
        let img = img.into_bgra8();
        let res = Res::from(img.dimensions());
        let stride: u32 = img
            .sample_layout()
            .height_stride
            .try_into()
            .expect("Corrupted image.");
        Self {
            buf: Arc::pin(ImgBuf(img.into_raw())),
            res,
            stride,
        }
    }
}

impl From<Bgra> for DynamicImage {
    fn from(bgra: Bgra) -> Self {
        // This does perform an expensive clone of the image data, but it's only ever done during
        // rescaling after the initial load, when we're guaranteed to have another owner.
        let container = (*bgra.buf).clone();
        Self::ImageBgra8(
            ImageBuffer::<image::Bgra<u8>, Vec<u8>>::from_vec(bgra.res.w, bgra.res.h, container)
                .expect("Conversion back to image buffer cannot fail"),
        )
    }
}

impl Bgra {
    pub fn as_ptr(&self) -> *const u8 {
        self.buf.as_ptr()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScaledImage {
    pub bgra: Bgra,
    pub original_res: Res,
}

#[derive(Debug, Eq, Clone)]
pub enum Displayable {
    Image(ScaledImage),
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
    Execute(String),
    ToggleUpscaling,
    ToggleManga,
}

#[derive(PartialEq, Eq, Copy, Clone)]
pub struct Res {
    pub w: u32,
    pub h: u32,
}

impl fmt::Debug for Res {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}x{}", self.w, self.h)
    }
}

impl From<(i32, i32)> for Res {
    fn from(wh: (i32, i32)) -> Self {
        if wh.0 < 0 || wh.1 < 0 {
            unreachable!("Can't have negative width or height");
        }

        Self {
            w: wh.0 as u32,
            h: wh.1 as u32,
        }
    }
}

impl From<(u32, u32)> for Res {
    fn from(wh: (u32, u32)) -> Self {
        Self { w: wh.0, h: wh.1 }
    }
}

impl From<DynamicImage> for Res {
    fn from(di: DynamicImage) -> Self {
        if let Some(fs) = di.as_flat_samples_u8() {
            (fs.layout.width, fs.layout.height).into()
        } else if let Some(fs) = di.as_flat_samples_u16() {
            (fs.layout.width, fs.layout.height).into()
        } else {
            unreachable!()
        }
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
    // Height,
    // Width,
    FullSize,
}

impl Default for Fit {
    fn default() -> Self {
        Self::Container
    }
}


#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TargetRes {
    pub res: Res,
    pub fit: Fit,
}

impl TargetRes {
    pub fn res_is_unset(&self) -> bool {
        self.res.is_zero()
    }
}

impl From<(i32, i32, Fit)> for TargetRes {
    fn from((w, h, fit): (i32, i32, Fit)) -> Self {
        Self {
            res: (w, h).into(),
            fit,
        }
    }
}

#[derive(Debug, PartialEq, Eq, Copy, Clone)]
pub struct LoadingParams {
    pub scale_during_load: bool,
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
}

#[derive(Debug)]
pub enum GuiAction {
    State(GuiState),
    Action(String, CommandResponder),
    Quit,
}
