use std::collections::hash_map::Entry;
use std::ops::Deref;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use std::{fmt, thread};

use ahash::AHashMap;
use derive_more::{Deref, From};
use gl::types::GLenum;
use image::{DynamicImage, GenericImageView};

use super::{DedupedVec, Res};
use crate::resample;

pub struct GLLayout {
    pub format: GLenum,
    pub swizzle: [u32; 4],
    pub alignment: i32,
}

enum ImageData {
    Rgba(Vec<u8>),
    Rgb(Vec<u8>),
    GreyA(Vec<u8>),
    Grey(Vec<u8>),
}

impl Deref for ImageData {
    type Target = Vec<u8>;

    fn deref(&self) -> &Self::Target {
        match self {
            Self::Rgba(v) | Self::Rgb(v) | Self::GreyA(v) | Self::Grey(v) => v,
        }
    }
}

impl Drop for ImageData {
    fn drop(&mut self) {
        // Cut down on the spam when animations are dropped
        if self.len() != 0 {
            trace!(
                "Cleaned up {:.2}MB in {}",
                (self.len() as f64) / 1_048_576.0,
                thread::current().name().unwrap_or("unknown")
            )
        }
    }
}

impl ImageData {
    #[inline]
    const fn channels(&self) -> usize {
        match self {
            Self::Rgba(_) => 4,
            Self::Rgb(_) => 3,
            Self::GreyA(_) => 2,
            Self::Grey(_) => 1,
        }
    }

    #[inline]
    const fn gl_layout(&self) -> &GLLayout {
        match self {
            Self::Rgba(_) => &GLLayout {
                format: gl::RGBA,
                swizzle: [gl::RED, gl::GREEN, gl::BLUE, gl::ALPHA],
                alignment: 4,
            },
            Self::Rgb(_) => &GLLayout {
                format: gl::RGB,
                swizzle: [gl::RED, gl::GREEN, gl::BLUE, gl::ALPHA],
                alignment: 1,
            },
            Self::GreyA(_) => &GLLayout {
                format: gl::RG,
                // GREEN will be copied to ALPHA after sRGB conversion, shader needs to deconvert
                swizzle: [gl::RED, gl::RED, gl::RED, gl::GREEN],
                alignment: 2,
            },
            Self::Grey(_) => &GLLayout {
                format: gl::RED,
                swizzle: [gl::RED, gl::RED, gl::RED, gl::ALPHA],
                alignment: 1,
            },
        }
    }

    #[inline]
    const fn grey(&self) -> bool {
        match self {
            Self::Rgba(_) | Self::Rgb(_) => false,
            Self::GreyA(_) | Self::Grey(_) => true,
        }
    }

    const fn format(&self) -> &str {
        match self {
            Self::Rgba(_) => "rgba",
            Self::Rgb(_) => "rgb",
            Self::GreyA(_) => "greya",
            Self::Grey(_) => "grey",
        }
    }

    // This is for clearing out the data for animations to avoid firing individual "Cleaned up XMB"
    // logs per frame.
    fn clear(&mut self) {
        match self {
            ImageData::Rgba(v) | ImageData::Rgb(v) | ImageData::GreyA(v) | ImageData::Grey(v) => {
                v.clear()
            }
        }
    }
}


#[derive(Clone)]
pub struct Image {
    data: Arc<ImageData>,
    pub res: Res,
    stride: usize,
}

impl PartialEq for Image {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.data, &other.data) && self.res == other.res && self.stride == other.stride
    }
}

impl Eq for Image {}

impl fmt::Debug for Image {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[Image: {} - {:?}]", self.data.format(), self.res)
    }
}

impl From<DynamicImage> for Image {
    fn from(img: DynamicImage) -> Self {
        let res = Res::from(img.dimensions());

        // Rust-analyzer bug
        #[cfg(not(feature = "benchmarking"))]
        if crate::config::CONFIG.force_rgba {
            // Could add optimized paths here as well, probably not really worth the code.
            let img = img.into_rgba8();
            return Self::from_rgba_buffer(img.into_vec(), res);
        }

        match img {
            DynamicImage::ImageLuma8(g) => {
                return Self::from_grey_buffer(g.into_vec(), res);
            }
            DynamicImage::ImageLuma16(_) => {
                let g = img.into_luma8();
                return Self::from_grey_buffer(g.into_vec(), res);
            }
            DynamicImage::ImageLumaA8(ga) => {
                return Self::from_grey_a_buffer(ga.into_vec(), res);
            }
            DynamicImage::ImageLumaA16(_) => {
                let ga = img.into_luma_alpha8();
                return Self::from_grey_a_buffer(ga.into_vec(), res);
            }
            DynamicImage::ImageRgb8(rgb) => {
                if rgb.chunks_exact(3).all(|c| c[0] == c[1] && c[1] == c[2]) {
                    let g = rgb.into_vec().into_iter().step_by(3).collect();
                    return Self::from_grey_buffer(g, res);
                }
                return Self::from_rgb_buffer(rgb.into_vec(), res);
            }
            DynamicImage::ImageRgb16(rgb) => {
                if rgb.chunks_exact(3).all(|c| c[0] == c[1] && c[1] == c[2]) {
                    let g = DynamicImage::ImageRgb16(rgb).into_luma8();
                    return Self::from_grey_buffer(g.into_vec(), res);
                }

                let rgb = DynamicImage::ImageRgb16(rgb).into_rgb8();
                return Self::from_rgb_buffer(rgb.into_vec(), res);
            }
            _ => {}
        }

        let img = img.into_rgba8();

        let is_grey = img.chunks_exact(4).all(|c| c[0] == c[1] && c[1] == c[2]);
        let opaque = img.chunks_exact(4).all(|c| c[3] == 255);
        match (is_grey, opaque) {
            (false, false) => Self::from_rgba_buffer(img.into_vec(), res),
            (true, true) => {
                let new_img = img.into_vec().into_iter().step_by(4).collect();
                Self::from_grey_buffer(new_img, res)
            }
            (false, true) => {
                let mut new_img = vec![0; img.as_raw().len() / 4 * 3];
                new_img.chunks_exact_mut(3).zip(img.chunks_exact(4)).for_each(|(nc, oc)| {
                    nc[0] = oc[0];
                    nc[1] = oc[1];
                    nc[2] = oc[2];
                });
                Self::from_rgb_buffer(new_img, res)
            }
            (true, false) => {
                let mut new_img = vec![0; img.as_raw().len() / 2];
                new_img.chunks_exact_mut(2).zip(img.chunks_exact(4)).for_each(|(nc, oc)| {
                    nc[0] = oc[0];
                    nc[1] = oc[3];
                });
                Self::from_grey_a_buffer(new_img, res)
            }
        }
    }
}

impl Image {
    pub fn as_ptr(&self) -> *const u8 {
        self.data.as_ptr()
    }

    // Get a pointer to an indiviual pixel, useful for rendering extremely large images with
    // offsets.
    pub fn as_offset_ptr(&self, x: u32, y: u32) -> *const u8 {
        std::ptr::addr_of!(self.data[y as usize * self.stride + x as usize * self.data.channels()])
    }

    fn from_rgba_buffer(img: Vec<u8>, res: Res) -> Self {
        let stride = res.w as usize * 4;
        let data = Arc::new(ImageData::Rgba(img));
        Self { data, res, stride }
    }

    fn from_rgb_buffer(img: Vec<u8>, res: Res) -> Self {
        let stride = res.w as usize * 3;
        let data = Arc::new(ImageData::Rgb(img));
        Self { data, res, stride }
    }

    fn from_grey_a_buffer(img: Vec<u8>, res: Res) -> Self {
        let stride = res.w as usize * 2;
        let data = Arc::new(ImageData::GreyA(img));
        Self { data, res, stride }
    }

    fn from_grey_buffer(img: Vec<u8>, res: Res) -> Self {
        let stride = res.w as usize;
        let data = Arc::new(ImageData::Grey(img));
        Self { data, res, stride }
    }

    pub fn downscale(&self, target_res: Res) -> Self {
        match &*self.data.as_ref() {
            ImageData::Rgba(v) => {
                let img = resample::resize_par_linear::<4>(
                    v,
                    self.res,
                    target_res,
                    resample::FilterType::CatmullRom,
                );
                Self::from_rgba_buffer(img, target_res)
            }
            ImageData::Rgb(v) => {
                let img = resample::resize_par_linear::<3>(
                    v,
                    self.res,
                    target_res,
                    resample::FilterType::CatmullRom,
                );
                Self::from_rgb_buffer(img, target_res)
            }
            ImageData::GreyA(v) => {
                let img = resample::resize_par_linear::<2>(
                    v,
                    self.res,
                    target_res,
                    resample::FilterType::CatmullRom,
                );
                Self::from_grey_a_buffer(img, target_res)
            }
            ImageData::Grey(v) => {
                let img = resample::resize_par_linear::<1>(
                    v,
                    self.res,
                    target_res,
                    resample::FilterType::CatmullRom,
                );
                Self::from_grey_buffer(img, target_res)
            }
        }
    }

    pub fn gl_layout(&self) -> &GLLayout {
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
        let sum = self.0.iter_deduped_mut().fold(0, |acc, f| {
            let x = f.0.data.len();
            if let Some(inner) = Arc::get_mut(&mut f.0.data) {
                // This should always succeed since we won't be cloning the inner arcs.
                inner.clear();
            } else {
                error!("Dropping AnimatedImage while some frames still have references");
            }
            acc + x
        });

        trace!(
            "Cleaned up {} frames, {:.2}MB in {}",
            count,
            sum as f64 / 1_048_576.0,
            thread::current().name().unwrap_or("unknown")
        )
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
                    let mut img = img;
                    if let Some(inner) = Arc::get_mut(&mut img.data) {
                        // This should always succeed at this point in time.
                        inner.clear();
                    } else {
                        error!(
                            "Frame somehow had multiple references during AnimatedImage \
                             construction."
                        );
                    }
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

#[derive(Debug, Default, PartialEq, Eq, Clone)]
pub enum Displayable {
    Image(ImageWithRes),
    Animation(AnimatedImage),
    Video(PathBuf),
    Error(String),
    Pending(Res),
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
