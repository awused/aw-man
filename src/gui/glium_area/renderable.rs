use std::cell::RefCell;
use std::cmp::Ordering;
use std::mem::ManuallyDrop;
use std::rc::{Rc, Weak};
use std::time::{Duration, Instant};

use derive_more::Deref;
use glium::Frame;
use gtk::glib::{self, SourceId};
use gtk::prelude::*;

use super::imp::RenderContext;
use crate::closing;
use crate::com::{AnimatedImage, DedupedVec, Displayable, ImageWithRes, Res};
use crate::gui::prog::Progress;
use crate::gui::{Gui, GUI};

mod static_image;
pub use static_image::*;

#[derive(Debug, PartialEq, Eq)]
pub(super) enum PreloadTask {
    Nothing,
    AnimationFrame(usize),
    WholeImage,
    Tiles(Vec<(usize, usize)>),
}

#[derive(Debug)]
enum AnimationStatus {
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
pub struct Animation {
    animated: AnimatedImage,
    textures: DedupedVec<StaticImage>,
    preload_textures: AllocatedTextures,
    index: usize,
    status: AnimationStatus,
}

impl Animation {
    pub fn new(
        a: &AnimatedImage,
        mut allocated: AllocatedTextures,
        play: bool,
    ) -> Rc<RefCell<Self>> {
        let textures = a.frames().map(|(img, _)| {
            StaticImage::new(
                ImageWithRes {
                    img: img.clone(),
                    file_res: img.res,
                    original_res: img.res,
                },
                std::mem::take(&mut allocated),
            )
        });

        Rc::new_cyclic(|weak| {
            let w = weak.clone();

            let status = if play {
                // We can safely assume this will be drawn as soon as possible.
                let target_time = Instant::now()
                    .checked_add(a.frames()[0].1)
                    .unwrap_or_else(|| Instant::now() + Duration::from_secs(1));

                let timeout_id =
                    ManuallyDrop::new(glib::timeout_add_local_once(a.frames()[0].1, move || {
                        Self::advance_animation(w)
                    }));
                AnimationStatus::Playing { target_time, timeout_id }
            } else {
                AnimationStatus::Paused(Duration::ZERO)
            };


            RefCell::new(Self {
                animated: a.clone(),
                textures,
                preload_textures: AllocatedTextures::default(),
                index: 0,
                status,
            })
        })
    }

    pub(super) fn setup_progress(&self, p: &mut Progress) {
        p.attach_animation(&self.animated);
        p.animation_tick(self.get_elapsed());
    }

    fn get_elapsed(&self) -> Duration {
        match &self.status {
            AnimationStatus::Playing { target_time, .. } => {
                let remaining = target_time.saturating_duration_since(Instant::now());
                let f = self.animated.frames();
                (f.cumulative_dur[self.index] + f[self.index].1).saturating_sub(remaining)
            }
            AnimationStatus::Paused(elapsed) => *elapsed,
        }
    }

    fn set_playing(rc: &Rc<RefCell<Self>>, play: bool) {
        let mut ac = rc.borrow_mut();

        match (play, &ac.status) {
            (false, AnimationStatus::Playing { .. }) => {
                let elapsed = ac.get_elapsed();

                GUI.with(|gui| gui.get().unwrap().progress.borrow_mut().animation_tick(elapsed));

                ac.status = AnimationStatus::Paused(elapsed);
            }
            (true, AnimationStatus::Paused(elapsed)) => {
                let weak = Rc::downgrade(rc);

                let frames = ac.animated.frames();
                let remaining =
                    (frames.cumulative_dur[ac.index] + frames[ac.index].1).saturating_sub(*elapsed);
                let target_time = Instant::now()
                    .checked_add(remaining)
                    .unwrap_or_else(|| Instant::now() + Duration::from_secs(1));

                let timeout_id =
                    ManuallyDrop::new(glib::timeout_add_local_once(remaining, move || {
                        Self::advance_animation(weak)
                    }));

                ac.status = AnimationStatus::Playing { target_time, timeout_id };
            }
            (true, AnimationStatus::Playing { .. }) | (false, AnimationStatus::Paused(..)) => (),
        }
    }

    fn advance_animation(weak: Weak<RefCell<Self>>) {
        let rc = weak.upgrade().unwrap();
        let mut ab = rc.borrow_mut();
        let ac = &mut *ab;

        let (target_time, timeout_id) =
            if let AnimationStatus::Playing { target_time, timeout_id } = &mut ac.status {
                (target_time, &mut **timeout_id)
            } else {
                unreachable!()
            };

        while *target_time < Instant::now() {
            ac.index = (ac.index + 1) % ac.animated.frames().len();
            let dur = ac.animated.frames()[ac.index].1;

            *target_time = target_time
                .checked_add(dur)
                .unwrap_or_else(|| Instant::now() + Duration::from_secs(1));
        }

        GUI.with(|gui| {
            let g = gui.get().unwrap();
            g.canvas.queue_draw();

            let mut pb = g.progress.borrow_mut();
            pb.animation_tick(ac.animated.frames().cumulative_dur[ac.index]);
        });

        *timeout_id = glib::timeout_add_local_once(
            target_time.saturating_duration_since(Instant::now()),
            move || Self::advance_animation(weak),
        );
    }

    pub(super) fn seek_animation(rc: &Rc<RefCell<Self>>, dur: Duration) {
        let mut ab = rc.borrow_mut();
        let ac = &mut *ab;

        let index = ac
            .animated
            .frames()
            .cumulative_dur
            .binary_search_by(|cd| cd.cmp(&dur).then(Ordering::Less))
            .unwrap_err();
        ac.index = index - 1;

        GUI.with(|gui| gui.get().unwrap().canvas.queue_draw());

        match &mut ac.status {
            AnimationStatus::Paused(elapsed) => {
                *elapsed = dur;
            }
            AnimationStatus::Playing { target_time, timeout_id } => {
                let remainder = (ac.animated.frames().cumulative_dur[ac.index]
                    + ac.animated.frames()[ac.index].1)
                    .saturating_sub(dur);

                *target_time = Instant::now()
                    .checked_add(remainder)
                    .unwrap_or_else(|| Instant::now() + Duration::from_secs(1));

                let weak = Rc::downgrade(rc);

                let old_timeout = std::mem::replace(
                    &mut **timeout_id,
                    glib::timeout_add_local_once(
                        target_time.saturating_duration_since(Instant::now()),
                        move || Self::advance_animation(weak),
                    ),
                );
                old_timeout.remove();
            }
        }
    }

    fn preload_frame(&mut self, ctx: &RenderContext, next: usize) {
        self.textures[next].attach_textures(std::mem::take(&mut self.preload_textures));
        self.textures[next].preload(ctx, Vec::new());
    }

    pub(super) fn invalidate(&mut self) {
        std::mem::take(&mut self.preload_textures);

        for t in self.textures.iter_deduped_mut() {
            t.invalidate();
        }
    }

    pub(super) fn take_textures(&mut self) -> AllocatedTextures {
        // TODO -- look at current and next/prev frame if there's nothing here.
        std::mem::take(&mut self.preload_textures)
    }

    pub(super) fn draw(
        &mut self,
        ctx: &RenderContext,
        frame: &mut Frame,
        layout: (i32, i32, Res),
        target_size: Res,
    ) -> (bool, PreloadTask) {
        let (drew, _) = self.textures[self.index].draw(ctx, frame, layout, target_size);

        // Only preload after a successful draw to avoid wasting effort on offscreen animations
        if !drew {
            return (false, PreloadTask::Nothing);
        }

        let next = (self.index + 1) % self.animated.frames().len();
        if self.textures[next].needs_preload() {
            let prev =
                if self.index == 0 { self.animated.frames().len() - 1 } else { self.index - 1 };

            if prev != next && prev != self.index {
                self.preload_textures = self.textures[prev].take_textures();
            }

            (true, PreloadTask::AnimationFrame(next))
        } else {
            (true, PreloadTask::Nothing)
        }
    }
}

// Vid and DropLabel are there so that Renderable is !Drop
#[derive(Debug, Deref)]
pub(super) struct Vid(ManuallyDrop<gtk::Video>);

impl Drop for Vid {
    fn drop(&mut self) {
        // Safe because we're dropping it
        let vid = unsafe { ManuallyDrop::take(&mut self.0) };
        vid.parent()
            .unwrap()
            .dynamic_cast::<gtk::Overlay>()
            .unwrap()
            .remove_overlay(&vid);
        vid.media_stream().unwrap().set_playing(false);

        // Add a short timeout so that we can be nearly certain the next image is
        // visible before we start to drop the video.
        glib::timeout_add_local_once(Duration::from_millis(10), move || {
            if closing::closed() {
                std::mem::forget(vid);
            } else {
                let start = Instant::now();
                drop(vid);

                if start.elapsed() > Duration::from_millis(5) {
                    trace!("Took {:?} to drop video.", start.elapsed());
                } else {
                    error!(
                        "Took {:?} to drop video, which is too short. GTK probably leaked it.",
                        start.elapsed()
                    );
                }
            }
        });
    }
}

#[derive(Debug, Deref)]
pub(super) struct DropLabel(gtk::Label);

impl Drop for DropLabel {
    fn drop(&mut self) {
        self.0
            .parent()
            .unwrap()
            .dynamic_cast::<gtk::Overlay>()
            .unwrap()
            .remove_overlay(&self.0)
    }
}

#[allow(clippy::large_enum_variant)]
#[derive(Debug, Default)]
pub(super) enum Renderable {
    #[default]
    Nothing,
    Pending(Res),
    Image(StaticImage),
    Animation(Rc<RefCell<Animation>>),
    Video(Vid),
    Error(DropLabel),
}

impl Renderable {
    pub(super) fn new(d: &Displayable, at: AllocatedTextures, g: &Rc<Gui>) -> Self {
        match d {
            Displayable::Image(img) => Self::Image(StaticImage::new(img.clone(), at)),
            Displayable::Animation(a) => {
                Self::Animation(Animation::new(a, at, g.animation_playing.get()))
            }
            Displayable::Video(v) => {
                // TODO -- preload video https://gitlab.gnome.org/GNOME/gtk/-/issues/4062
                let mf = gtk::MediaFile::for_filename(&*v.to_string_lossy());
                mf.set_loop(true);
                mf.set_playing(g.animation_playing.get());

                let vid = gtk::Video::new();

                vid.set_halign(gtk::Align::Center);
                vid.set_valign(gtk::Align::Center);

                vid.set_hexpand(false);
                vid.set_vexpand(false);

                vid.set_autoplay(true);
                vid.set_loop(true);

                vid.set_focusable(false);
                vid.set_can_focus(false);

                vid.set_media_stream(Some(&mf));

                g.overlay.add_overlay(&vid);

                Self::Video(Vid(ManuallyDrop::new(vid)))
            }
            Displayable::Error(e) => {
                let error = gtk::Label::new(Some(e));

                error.set_halign(gtk::Align::Center);
                error.set_valign(gtk::Align::Center);

                error.set_hexpand(false);
                error.set_vexpand(false);

                error.set_wrap(true);
                error.set_max_width_chars(120);
                error.add_css_class("error-label");

                g.overlay.add_overlay(&error);

                Self::Error(DropLabel(error))
            }
            Displayable::Pending => Self::Nothing,
            Displayable::Loading { file_res, .. } => Self::Pending(*file_res),
        }
    }

    fn set_playing(&mut self, play: bool) {
        match self {
            Self::Animation(a) => Animation::set_playing(a, play),
            Self::Video(v) => {
                let ms = v.media_stream().unwrap();
                match (play, ms.is_playing()) {
                    (false, true) => ms.set_playing(false),
                    (true, false) => ms.set_playing(true),
                    (true, true) | (false, false) => (),
                }
            }
            Self::Image(_) | Self::Pending { .. } | Self::Error(_) | Self::Nothing => {}
        }
    }

    pub(super) fn matches(&self, disp: &Displayable) -> bool {
        match (self, disp) {
            (Self::Image(si), Displayable::Image(di)) => &si.image == di,
            (Self::Animation(sa), Displayable::Animation(da)) => &sa.borrow().animated == da,
            (Self::Video(_sv), Displayable::Video(_dv)) => {
                error!("Videos cannot be equal yet");
                false
            }
            (Self::Error(se), Displayable::Error(de)) => se.text().as_str() == de,
            (Self::Pending(sr), Displayable::Loading { file_res: dr, .. }) => sr == dr,
            (Self::Nothing, Displayable::Pending) => true,
            (
                Self::Image(_)
                | Self::Animation(_)
                | Self::Video(_)
                | Self::Error(_)
                | Self::Pending { .. }
                | Self::Nothing,
                _,
            ) => false,
        }
    }

    fn invalidate(&mut self) {
        match self {
            Self::Nothing | Self::Pending { .. } | Self::Video(_) | Self::Error(_) => (),
            Self::Image(i) => i.invalidate(),
            Self::Animation(a) => a.borrow_mut().invalidate(),
        }
    }

    pub(super) fn take_textures(&mut self) -> AllocatedTextures {
        match self {
            Self::Image(i) => i.take_textures(),
            Self::Animation(a) => a.borrow_mut().take_textures(),
            Self::Nothing | Self::Pending { .. } | Self::Video(_) | Self::Error(_) => {
                AllocatedTextures::default()
            }
        }
    }

    // Returns whether it drew something, and whether it wants some preloading
    pub(super) fn draw(
        &mut self,
        ctx: &RenderContext,
        frame: &mut Frame,
        layout: (i32, i32, Res),
        target_size: Res,
    ) -> (bool, PreloadTask) {
        match self {
            Self::Image(img) => img.draw(ctx, frame, layout, target_size),
            Self::Animation(a) => a.borrow_mut().draw(ctx, frame, layout, target_size),
            Self::Nothing | Self::Pending { .. } | Self::Video(_) | Self::Error(_) => {
                (false, PreloadTask::Nothing)
            }
        }
    }

    pub(super) fn preload(&mut self, ctx: &RenderContext, task: PreloadTask) {
        match task {
            PreloadTask::Nothing => {}
            PreloadTask::AnimationFrame(index) => {
                let Self::Animation(a) = self else {
                    unreachable!()
                };
                a.borrow_mut().preload_frame(ctx, index);
            }
            PreloadTask::WholeImage => {
                let Self::Image(img) = self else {
                    unreachable!()
                };
                img.preload(ctx, Vec::new())
            }
            PreloadTask::Tiles(tiles) => {
                let Self::Image(img) = self else {
                    unreachable!()
                };
                img.preload(ctx, tiles)
            }
        }
    }
}


#[allow(clippy::large_enum_variant)]
#[derive(Debug)]
pub(super) enum DisplayedContent {
    Single(Renderable),
    Multiple(Vec<Renderable>),
}

impl Default for DisplayedContent {
    fn default() -> Self {
        Self::Single(Renderable::default())
    }
}

impl DisplayedContent {
    pub(super) fn set_playing(&mut self, play: bool) {
        match self {
            Self::Single(r) => r.set_playing(play),
            Self::Multiple(visible) => {
                for v in visible {
                    v.set_playing(play)
                }
            }
        }
    }

    pub(super) fn drop_textures(&mut self) {
        match self {
            Self::Single(r) => r.invalidate(),
            Self::Multiple(visible) => {
                for v in visible {
                    v.invalidate()
                }
            }
        }
    }

    pub(super) fn seek_animation(&self, dur: Duration) {
        match self {
            Self::Single(Renderable::Animation(a)) => {
                Animation::seek_animation(a, dur);
            }
            Self::Single(_) | Self::Multiple(_) => unreachable!(),
        }
    }
}

// For now this is only an optimization for the most common case: the next static image.
#[derive(Debug, Default)]
pub(super) enum Preloadable {
    #[default]
    Nothing,
    Pending(StaticImage),
    Loaded(StaticImage),
    // This would be complicated
    // Dual(StaticImage, StaticImage),
}

impl From<Renderable> for Preloadable {
    fn from(r: Renderable) -> Self {
        if let Renderable::Image(img) = r {
            if img.needs_preload() { Self::Pending(img) } else { Self::Loaded(img) }
        } else {
            Self::Nothing
        }
    }
}

impl Preloadable {
    pub(super) fn new(disp: &Option<Displayable>, at: AllocatedTextures) -> Self {
        if let Some(Displayable::Image(img)) = disp {
            Self::Pending(StaticImage::new(img.clone(), at))
        } else {
            Self::Nothing
        }
    }

    pub(super) fn take_renderable(&mut self) -> Renderable {
        match std::mem::take(self) {
            Self::Nothing => Renderable::default(),
            Self::Pending(i) | Self::Loaded(i) => Renderable::Image(i),
        }
    }

    pub(super) fn drop_textures(&mut self) {
        match std::mem::take(self) {
            Self::Nothing => {}
            Self::Pending(mut img) | Self::Loaded(mut img) => {
                img.invalidate();
                *self = Self::Pending(img);
            }
        }
    }

    pub(super) const fn needs_preload(&self) -> bool {
        match self {
            Self::Nothing | Self::Loaded(..) => false,
            Self::Pending(..) => true,
        }
    }

    pub(super) fn preload(&mut self, ctx: &RenderContext) {
        match std::mem::take(self) {
            Self::Nothing => (),
            Self::Loaded(img) => *self = Self::Loaded(img),
            Self::Pending(mut img) => {
                img.preload(ctx, Vec::new());
                *self = Self::Loaded(img);
            }
        }
    }

    pub(super) fn matches(&self, disp: &Displayable) -> bool {
        match (self, disp) {
            (Self::Pending(si) | Self::Loaded(si), Displayable::Image(di)) => &si.image == di,
            (Self::Nothing | Self::Pending(_) | Self::Loaded(_), _) => false,
        }
    }
}
