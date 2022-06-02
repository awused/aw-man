use std::fmt;
use std::path::PathBuf;
use std::rc::Rc;

use tempfile::TempDir;
use tokio::fs::remove_file;
use Kind::*;

use super::animation::Animation;
use super::regular_image::RegularImage;
use super::upscaled_image::UpscaledImage;
use super::video::Video;
use super::Page;
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
        regpath: &Rc<PathBuf>,
        temp_dir: &Rc<TempDir>,
        index: usize,
    ) -> Self {
        let scale = bor.should_upscale();
        let res = bor.res();
        let r = RegularImage::new(bor, Rc::downgrade(regpath));
        if scale {
            let upath = format!("{index}-upscaled.png");
            let upath = temp_dir.path().join(upath);
            let u = UpscaledImage::new(upath, Rc::downgrade(regpath), res);
            Self::Image(r, u)
        } else {
            Self::UnupscaledImage(r)
        }
    }

    fn new_animation(regpath: &Rc<PathBuf>, res: Res) -> Self {
        Self::Animation(Animation::new(Rc::downgrade(regpath), res))
    }

    fn new_video(regpath: &Rc<PathBuf>) -> Self {
        debug!("todo video");
        Self::Video(Video::new(Rc::downgrade(regpath)))
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
    converted_file: Option<Rc<PathBuf>>,
}

impl ScannedPage {
    pub(super) fn new(page: &Page, sr: ScanResult) -> Self {
        use ScanResult as SR;

        let converted_file = match &sr {
            SR::ConvertedImage(pb, _) => Some(Rc::from(pb.clone())),
            SR::Image(_) | SR::Invalid(_) | SR::Animation(_) | SR::Video => None,
        };

        let kind = match sr {
            SR::ConvertedImage(_, bor) => {
                let regpath = converted_file.as_ref().expect("Impossible");
                Kind::new_image(bor, regpath, &page.temp_dir, page.index)
            }
            SR::Image(bor) => {
                Kind::new_image(bor, page.get_absolute_file_path(), &page.temp_dir, page.index)
            }
            SR::Animation(res) => Kind::new_animation(page.get_absolute_file_path(), res),
            SR::Video => Kind::new_video(page.get_absolute_file_path()),
            SR::Invalid(s) => Invalid(s),
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
