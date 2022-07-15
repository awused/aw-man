use std::fmt;

use derive_more::Display;
use image::DynamicImage;

use super::DisplayMode;

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
        assert!(wh.0 >= 0 && wh.1 >= 0, "Can't have negative width or height");

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
        let (tw, th) = if !t.half_width { (t.res.w, t.res.h) } else { (t.res.w / 2, t.res.h) };

        let scale = match t.fit {
            Fit::Container => f64::min(tw as f64 / w, th as f64 / h),
            Fit::Height => th as f64 / h,
            Fit::Width => tw as f64 / w,
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

#[derive(Debug, Display, Default, Clone, Copy, PartialEq, Eq)]
pub enum Fit {
    #[default]
    Container,
    Height,
    Width,
    FullSize,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct TargetRes {
    pub res: Res,
    pub fit: Fit,
    // Whether to force pages to be half their size
    half_width: bool,
}

impl From<(i32, i32, Fit, DisplayMode)> for TargetRes {
    fn from((w, h, fit, d): (i32, i32, Fit, DisplayMode)) -> Self {
        let half_width = d.half_width_pages();
        Self { res: (w, h).into(), fit, half_width }
    }
}

impl From<(u32, u32, Fit, DisplayMode)> for TargetRes {
    fn from((w, h, fit, d): (u32, u32, Fit, DisplayMode)) -> Self {
        let half_width = d.half_width_pages();
        Self { res: (w, h).into(), fit, half_width }
    }
}

impl From<(Res, Fit, DisplayMode)> for TargetRes {
    fn from((res, fit, d): (Res, Fit, DisplayMode)) -> Self {
        let half_width = d.half_width_pages();
        Self { res, fit, half_width }
    }
}
