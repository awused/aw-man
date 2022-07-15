use std::collections::hash_map::Entry;
use std::ops::Deref;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;
use std::{fmt, thread};

use ahash::AHashMap;
use derive_more::{Deref, From};
use gl::types::GLenum;
use image::{DynamicImage, RgbaImage};

use super::{DedupedVec, Res};
use crate::resample;

// OpenCL doesn't like RGB and transparent Greyscale is too rare to bother working at.
enum ImageData {
    Rgba(Vec<u8>),
    Grey(Vec<u8>),
}

impl Deref for ImageData {
    type Target = Vec<u8>;

    fn deref(&self) -> &Self::Target {
        match self {
            Self::Rgba(v) | Self::Grey(v) => v,
        }
    }
}

impl Drop for ImageData {
    fn drop(&mut self) {
        trace!(
            "Cleaned up {:.2}MB in {}",
            (self.len() as f64) / 1_048_576.0,
            thread::current().name().unwrap_or("unknown")
        )
    }
}

impl ImageData {
    #[inline]
    const fn channels(&self) -> usize {
        match self {
            Self::Rgba(_) => 4,
            Self::Grey(_) => 1,
        }
    }

    #[inline]
    const fn gl_layout(&self) -> (GLenum, GLenum) {
        match self {
            ImageData::Rgba(_) => (gl::RGBA, gl::UNSIGNED_INT_8_8_8_8_REV),
            ImageData::Grey(_) => (gl::RED, gl::UNSIGNED_BYTE),
        }
    }

    #[inline]
    const fn grey(&self) -> bool {
        match self {
            Self::Rgba(_) => false,
            Self::Grey(_) => true,
        }
    }
}


#[derive(Clone)]
pub struct Image {
    // Explicitly pinning is likely to be unnecessary, but not harmful.
    data: Pin<Arc<ImageData>>,
    pub res: Res,
    stride: u32,
}

impl PartialEq for Image {
    fn eq(&self, other: &Self) -> bool {
        self.data.as_ptr() == other.data.as_ptr()
    }
}
impl Eq for Image {}

impl fmt::Debug for Image {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[Image: {:?}, grey: {}]", self.res, self.grey())
    }
}

impl From<DynamicImage> for Image {
    fn from(img: DynamicImage) -> Self {
        match img {
            DynamicImage::ImageLuma8(g) => {
                let res = Res::from(g.dimensions());
                return Self::from_grey_buffer(g.into_vec(), res);
            }
            DynamicImage::ImageLuma16(_) => {
                let g = img.into_luma8();
                let res = Res::from(g.dimensions());
                return Self::from_grey_buffer(g.into_vec(), res);
            }
            _ => {}
        }

        let img = img.into_rgba8();
        let res = Res::from(img.dimensions());

        if img.chunks_exact(4).all(|c| c[0] == c[1] && c[1] == c[2] && c[3] == 255) {
            let new_img = img.chunks_exact(4).map(|c| c[0]).collect();
            return Self::from_grey_buffer(new_img, res);
        }

        Self::from_rgba_buffer(img, res)
    }
}

impl Image {
    pub fn as_ptr(&self) -> *const u8 {
        self.data.as_ptr()
    }

    // Get a pointer to an indiviual pixel, useful for rendering extremely large images with
    // offsets.
    pub fn as_offset_ptr(&self, x: u32, y: u32) -> *const u8 {
        std::ptr::addr_of!(
            self.data[y as usize * self.stride as usize + x as usize * self.data.channels()]
        )
    }

    fn from_grey_buffer(img: Vec<u8>, res: Res) -> Self {
        let stride = res.w;

        let data = Arc::pin(ImageData::Grey(img));
        Self { data, res, stride }
    }

    fn from_rgba_buffer(img: RgbaImage, res: Res) -> Self {
        let stride = img
            .sample_layout()
            .height_stride
            .try_into()
            .expect("Image corrupted or too large.");

        let data = Arc::pin(ImageData::Rgba(img.into_vec()));
        Self { data, res, stride }
    }

    pub fn downscale(&self, target_res: Res) -> Self {
        match &*self.data.as_ref() {
            ImageData::Rgba(v) => {
                let img = resample::resize_par_linear_rgba(
                    v,
                    self.res,
                    target_res,
                    resample::FilterType::CatmullRom,
                );
                Self::from_rgba_buffer(img, target_res)
            }
            ImageData::Grey(v) => {
                let img = resample::resize_par_linear_grey(
                    v,
                    self.res,
                    target_res,
                    resample::FilterType::CatmullRom,
                );
                Self::from_grey_buffer(img, target_res)
            }
        }
    }

    pub fn gl_layout(&self) -> (GLenum, GLenum) {
        self.data.gl_layout()
    }

    pub fn grey(&self) -> bool {
        self.data.grey()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageWithRes {
    pub img: Image,
    pub original_res: Res,
}

#[derive(Deref, From)]
pub struct Frames(DedupedVec<(Image, Duration)>);

impl Drop for Frames {
    fn drop(&mut self) {
        let count = self.0.len();
        trace!("Cleaned up {} frames", count)
    }
}

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
    pub fn new(frames: Vec<(Image, Duration, u64)>) -> Self {
        assert!(!frames.is_empty());

        let dur = frames.iter().fold(Duration::ZERO, |dur, frame| dur.saturating_add(frame.1));

        let mut hashed_frames: AHashMap<u64, usize> = AHashMap::new();
        let mut deduped_frames = 0;
        let mut deduped_bytes = 0;

        let mut index = 0;
        let mut deduped = Vec::new();
        let mut indices = Vec::new();

        for (img, dur, hash) in frames {
            match hashed_frames.entry(hash) {
                Entry::Occupied(e) => {
                    indices.push(*e.get());
                    deduped_bytes += img.data.len();
                    deduped_frames += 1;
                }
                Entry::Vacant(e) => {
                    indices.push(index);
                    deduped.push((img, dur));
                    e.insert(index);
                    index += 1;
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

        let frames: Frames = DedupedVec { deduped, indices }.into();

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

#[derive(Debug, Default, PartialEq, Eq, Clone)]
pub enum Displayable {
    Image(ImageWithRes),
    Animation(AnimatedImage),
    Video(PathBuf),
    Error(String),
    Pending(Res), // Generally for loading.
    #[default]
    Nothing,
}

impl Displayable {
    // The original resolution, before fitting, if scrolling is enabled for this type.
    pub fn layout_res(&self) -> Option<Res> {
        match self {
            Self::Image(ImageWithRes { original_res: res, .. }) | Self::Pending(res) => Some(*res),
            Self::Animation(a) => Some(a.frames()[0].0.res),
            Self::Video(_) | Self::Error(_) | Self::Nothing => None,
        }
    }
}
