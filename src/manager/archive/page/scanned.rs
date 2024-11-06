use std::fmt;
use std::path::Path;
use std::rc::Rc;
use std::sync::Arc;

use Kind::*;
use tempfile::TempDir;
use tokio::fs::remove_file;

use super::Page;
use super::animation::Animation;
use super::regular_image::RegularImage;
use super::upscaled_image::UpscaledImage;
use super::video::Video;
use crate::com::{Displayable, Res};
use crate::manager::archive::{Completion, Work};
use crate::pools::loading::{ImageOrRes, ScanResult};

enum Kind {
    Image(RegularImage, UpscaledImage),
    UnupscaledImage(RegularImage),
    Animation(Animation),
    Video(Video),
    Invalid(String),
}

impl Kind {
    fn new_image(
        bor: ImageOrRes,
        regpath: &Arc<Path>,
        temp_dir: &Rc<TempDir>,
        index: usize,
    ) -> Self {
        let scale = bor.should_upscale();
        let res = bor.res();
        let r = RegularImage::new(bor, Arc::downgrade(regpath));
        if scale {
            let upath = format!("{index}-upscaled.png");
            let upath = temp_dir.path().join(upath).into();
            let u = UpscaledImage::new(upath, Arc::downgrade(regpath), res);
            Self::Image(r, u)
        } else {
            Self::UnupscaledImage(r)
        }
    }

    fn new_animation(regpath: &Arc<Path>, res: Res) -> Self {
        Self::Animation(Animation::new(Arc::downgrade(regpath), res))
    }

    fn new_video(regpath: &Arc<Path>) -> Self {
        debug!("todo video");
        Self::Video(Video::new(Arc::downgrade(regpath)))
    }
}

impl fmt::Debug for Kind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Image(..) => "Image",
            UnupscaledImage(_) => "UnupscaledImage",
            Animation(_) => "Animation",
            Video(_) => "Video",
            Invalid(s) => s,
        };
        write!(f, "{s}")
    }
}

#[derive(Debug)]
pub(super) struct ScannedPage {
    kind: Kind,

    // This ScannedPage owns this file, if it exists.
    // This really could be an Rc instead of an Arc but it's not worth the code
    converted_file: Option<Arc<Path>>,
}

impl ScannedPage {
    pub(super) fn new(page: &Page, sr: ScanResult) -> Self {
        use ScanResult as SR;

        let (kind, converted_file) = match sr {
            SR::ConvertedImage(p, bor) if bor.res().is_empty() => {
                (Invalid("Empty image".to_string()), Some(p))
            }
            SR::Image(bor) if bor.res().is_empty() => (Invalid("Empty image".to_string()), None),
            SR::ConvertedImage(p, bor) => {
                (Kind::new_image(bor, &p, &page.temp_dir, page.index), Some(p))
            }
            SR::Image(bor) => (
                Kind::new_image(bor, page.get_absolute_file_path(), &page.temp_dir, page.index),
                None,
            ),
            SR::Animation(res) if res.is_empty() => (Invalid("Empty animation".to_string()), None),
            SR::Animation(res) => (Kind::new_animation(page.get_absolute_file_path(), res), None),
            SR::Video => (Kind::new_video(page.get_absolute_file_path()), None),
            SR::Invalid(s) => (Invalid(s), None),
        };

        Self { kind, converted_file }
    }

    pub(super) fn get_displayable(&self, upscaling: bool) -> Displayable {
        match &self.kind {
            Image(r, u) => {
                if upscaling {
                    u.get_displayable()
                } else {
                    r.get_displayable(None)
                }
            }
            UnupscaledImage(r) => r.get_displayable(None),
            Animation(a) => a.get_displayable(),
            Video(v) => v.get_displayable(),
            Invalid(e) => Displayable::Error(e.clone()),
        }
    }

    pub(super) fn has_work(&self, work: &Work) -> bool {
        match &work {
            Work::Finalize(..) | Work::Downscale(..) | Work::Load(..) | Work::Upscale => (),
            Work::Scan => return false,
        }

        match &self.kind {
            Image(r, u) => {
                if work.upscale() {
                    u.has_work(work)
                } else {
                    r.has_work(work)
                }
            }
            UnupscaledImage(r) => r.has_work(work),
            Animation(a) => a.has_work(work),
            Video(v) => v.has_work(work),
            Invalid(_) => false,
        }
    }

    // These functions should return after each level of work is complete.
    pub(super) async fn do_work(&mut self, work: Work<'_>) -> Completion {
        if work == Work::Scan {
            unreachable!("Tried to do scanning work on a ScannedPage.");
        }

        match &mut self.kind {
            Image(r, u) => {
                if work.upscale() {
                    u.do_work(work).await
                } else {
                    r.do_work(work).await
                }
            }
            UnupscaledImage(r) => r.do_work(work).await,
            Animation(a) => a.do_work(work).await,
            Video(v) => v.do_work(work).await,
            Invalid(_) => unreachable!("Tried to do work on an invalid scanned page."),
        }
        Completion::More
    }

    pub(super) async fn join(self) {
        match self.kind {
            Image(r, u) => {
                r.join().await;
                u.join().await;
            }
            UnupscaledImage(r) => r.join().await,
            Animation(a) => a.join().await,
            Video(v) => v.join().await,
            Invalid(_) => (),
        }

        if let Some(p) = self.converted_file {
            if let Err(e) = remove_file(p.as_ref()).await {
                error!("Failed to remove converted file {p:?}: {e:?}")
            }
        }
    }

    pub(super) fn unload(&mut self) {
        match &mut self.kind {
            Image(r, u) => {
                r.unload();
                u.unload();
            }
            UnupscaledImage(r) => r.unload(),
            Animation(a) => a.unload(),
            Video(v) => v.unload(),
            Invalid(_) => (),
        }
    }
}
