use std::cell::RefCell;
use std::cmp::min;
use std::mem::ManuallyDrop;
use std::rc::{Rc, Weak};
use std::time::{Duration, Instant};

use cgmath::{Matrix4, Ortho, Vector3};
use gdk4_x11::glib::clone::Downgrade;
use glium::backend::Facade;
use glium::texture::{MipmapsOption, SrgbTexture2d as _unused, Texture2d};
use glium::uniforms::{
    MagnifySamplerFilter, MinifySamplerFilter, Sampler, SamplerBehavior, SamplerWrapFunction,
};
use glium::{
    uniform, Blend, BlendingFunction, DrawParameters, Frame, GlObject, LinearBlendingFactor,
    Surface,
};
use gtk::glib::{self, SourceId};
use gtk::traits::WidgetExt;

use super::imp::Renderer;
use crate::com::{AnimatedImage, Bgra, DedupedVec, Displayable, Res, ScaledImage};
use crate::gui::GUI;

static TILE_SIZE: u32 = 512;
static MAX_UNTILED_SIZE: u32 = 8192;
// static MAX_UNTILED_SIZE: u32 = 16384;
// The minimum scale size we allow at render time. Set to avoid uploading extraordinarily large
// images in their entirety before they've had a chance to be downscaled by the CPU.
static MIN_AUTO_ZOOM: f32 = 0.2;

static BLEND_PARAMS: Blend = Blend {
    color: BlendingFunction::Addition {
        source: LinearBlendingFactor::SourceAlpha,
        destination: LinearBlendingFactor::OneMinusSourceAlpha,
    },
    alpha: BlendingFunction::Addition {
        source: LinearBlendingFactor::One,
        destination: LinearBlendingFactor::OneMinusSourceAlpha,
    },
    constant_value: (0.0, 0.0, 0.0, 0.0),
};


#[derive(Default)]
pub enum AllocatedTextures {
    #[default]
    Nothing,
    Single(Texture2d),
    Tiles(Vec<Texture2d>),
}

#[derive(Debug)]
enum TextureLayout {
    Single {
        current: Option<Texture2d>,
        previous: Option<Texture2d>,
    },
    Tiled {
        tiles: Vec<Vec<Option<Texture2d>>>,
        columns: f32,
        rows: f32,
        // TODO -- reuse these between textures
        // TODO -- unload these during idle_unload
        reuse_cache: Vec<Texture2d>,
    },
}

#[derive(Debug)]
pub struct StaticImage {
    texture: TextureLayout,
    image: ScaledImage,
}

#[derive(Debug, PartialEq, Eq)]
enum Visibility {
    Visible,
    Offscreen,
    // If it's more than one full tile offscreen, it can be unloaded
    UnloadTile,
}

fn is_visible_1d(
    inset: u32,
    dimension: u32,
    scale: f32,
    offset: i32,
    target_dim: u32,
) -> Visibility {
    let real_start = (inset as f32 * scale).floor() as i32 + offset;
    let real_end = ((inset + dimension) as f32 * scale).ceil() as i32 + offset;
    let real_tile = (TILE_SIZE as f32 * scale).ceil() as i32;


    if real_start - real_tile > target_dim as i32 || real_end + real_tile < 0 {
        Visibility::UnloadTile
    } else if real_start > target_dim as i32 || real_end < 0 {
        Visibility::Offscreen
    } else {
        Visibility::Visible
    }
}

impl StaticImage {
    pub fn new(image: ScaledImage, previous_textures: AllocatedTextures) -> Self {
        let c_res = image.bgra.res;
        let texture = if c_res.h <= MAX_UNTILED_SIZE && c_res.w <= MAX_UNTILED_SIZE {
            let previous = if let AllocatedTextures::Single(tex) = previous_textures {
                Some(tex)
            } else {
                None
            };
            TextureLayout::Single { current: None, previous }
        } else {
            let rows = c_res.h as f32 / TILE_SIZE as f32;
            let columns = c_res.w as f32 / TILE_SIZE as f32;

            let mut tiles = Vec::with_capacity(rows.ceil() as usize);
            tiles.resize_with(tiles.capacity(), || {
                let mut row = Vec::with_capacity(columns.ceil() as usize);
                row.resize_with(row.capacity(), || None);
                row
            });

            let reuse_cache = if let AllocatedTextures::Tiles(tiles) = previous_textures {
                tiles
            } else {
                Vec::new()
            };

            TextureLayout::Tiled { tiles, columns, rows, reuse_cache }
        };


        Self { texture, image }
    }

    fn upload_whole_image(
        bgra: &Bgra,
        display: &Renderer,
        existing: Option<Texture2d>,
    ) -> Texture2d {
        let w = bgra.res.w;
        let h = bgra.res.h;

        let start = Instant::now();
        let tex = match existing {
            Some(tex) if tex.width() == w && tex.height() == h => tex,
            _ => Texture2d::empty_with_mipmaps(&display, MipmapsOption::NoMipmap, w, h).unwrap(),
        };


        unsafe {
            display.get_context().exec_in_context(|| {
                gl::TextureSubImage2D(
                    tex.get_id(),
                    0,
                    0,
                    0,
                    w as i32,
                    h as i32,
                    gl::RGBA,
                    gl::UNSIGNED_INT_8_8_8_8_REV,
                    bgra.as_ptr() as *const libc::c_void,
                );

                println!("upload: {:?}", start.elapsed());
            });
        }
        tex
    }

    fn upload_tile(
        bgra: &Bgra,
        display: &Renderer,
        x: u32,
        y: u32,
        existing: Option<Texture2d>,
    ) -> Texture2d {
        let width = min(TILE_SIZE, bgra.res.w - x);
        let height = min(TILE_SIZE, bgra.res.h - y);

        let start = Instant::now();
        let mut new = false;
        // Fixed size tiles, at least for now
        let tex = existing.unwrap_or_else(|| {
            new = true;
            Texture2d::empty_with_mipmaps(&display, MipmapsOption::NoMipmap, TILE_SIZE, TILE_SIZE)
                .unwrap()
        });

        unsafe {
            display.get_context().exec_in_context(|| {
                gl::PixelStorei(gl::UNPACK_ROW_LENGTH, bgra.res.w as i32);

                static TILE_BG: [u8; 4] = [0u8, 0xff, 0, 0xff];

                gl::ClearTexImage(
                    tex.get_id(),
                    0,
                    gl::RGBA,
                    gl::UNSIGNED_BYTE,
                    std::ptr::addr_of!(TILE_BG) as *const _,
                );

                gl::TextureSubImage2D(
                    tex.get_id(),
                    0,
                    0,
                    0,
                    width as i32,
                    height as i32,
                    gl::RGBA,
                    gl::UNSIGNED_INT_8_8_8_8_REV,
                    bgra.as_offset_ptr(x, y) as *const libc::c_void,
                );

                gl::PixelStorei(gl::UNPACK_ROW_LENGTH, 0);
                println!("upload tile: {:?}, new: {new}", start.elapsed());
            });
        }
        tex
    }

    // Returns true if this should be preloaded for animation, which is limited to untiled images.
    const fn needs_animation_preload(&self) -> bool {
        matches!(&self.texture, TextureLayout::Single { current: None, .. })
    }

    fn preload(&mut self, display: &Renderer) {
        if let TextureLayout::Single { current, previous } = &mut self.texture {
            if current.is_none() {
                *current =
                    Some(Self::upload_whole_image(&self.image.bgra, display, previous.take()));
            }
        }
    }

    pub fn take_textures(&mut self) -> AllocatedTextures {
        // Take a texture for reuse for the next image, if possible.
        // Currently only for images below the cutoff size for single tiles.
        match &mut self.texture {
            TextureLayout::Single { current, .. } => {
                current.take().map_or(AllocatedTextures::Nothing, AllocatedTextures::Single)
            }
            TextureLayout::Tiled { tiles, reuse_cache, .. } => {
                let mut reuse_cache = std::mem::take(reuse_cache);
                let tiles = std::mem::take(tiles);
                for t in tiles.into_iter().flatten().flatten() {
                    reuse_cache.push(t);
                }
                AllocatedTextures::Tiles(reuse_cache)
            }
        }
    }

    pub fn invalidate(&mut self) {
        match &mut self.texture {
            TextureLayout::Single { current, previous } => {
                *current = None;
                *previous = None;
            }
            TextureLayout::Tiled {
                tiles,
                reuse_cache,
                columns: _columns,
                rows: _rows,
            } => {
                for row in tiles {
                    for t in row {
                        *t = None;
                    }
                }

                reuse_cache.clear();
            }
        }
    }

    fn render_matrix(
        target: Res,
        width: f32,
        height: f32,
        scale: f32,
        // Offset from top left in display coordinates
        offsets: (i32, i32),
    ) -> Matrix4<f32> {
        let matrix: Matrix4<f32> = Ortho {
            left: 0.0,
            right: target.w as f32 / width * 2.0,
            bottom: (target.h as f32 / height) * -2.0,
            top: 0.0,
            near: 0.0,
            far: 1.0,
        }
        .into();

        let (ofx, ofy) = offsets;

        let scale_m = Matrix4::from_nonuniform_scale(scale, -scale, 1.0);
        let upper_left = Matrix4::from_translation(Vector3::new(
            (1.0 - 0.5 / width as f32) * scale,
            (-1.0 + 0.5 / height as f32) * scale,
            0.0,
        ));
        let offset = Matrix4::from_translation(Vector3::new(
            (ofx as f32 * 2.0) / width / scale,
            (ofy as f32 * 2.0) / height / scale,
            0.0,
        ));

        matrix * upper_left * scale_m * offset
    }

    pub(super) fn draw(
        &mut self,
        display: &Renderer,
        frame: &mut Frame,
        layout: (i32, i32, Res),
        target_size: Res,
    ) {
        let start = Instant::now();
        let (ofx, ofy, res) = layout;
        if res.is_zero_area() {
            warn!("Attempted to draw 0 sized image");
            return;
        }

        let current_res = self.image.bgra.res;

        let scale = if res.w == current_res.w {
            1.
        } else {
            debug!("Needed to scale image at draw time. {:?} -> {:?}", current_res, res);
            res.w as f32 / current_res.w as f32
        };

        if scale < MIN_AUTO_ZOOM {
            warn!(
                "Skipping rendering since scale ({scale:?}) was too low (minimum: \
                 {MIN_AUTO_ZOOM:?}). Waiting for CPU downscaling."
            );
            return;
        }

        // We initialize it to 1.0 earlier, so it will normally be exactly 1.0
        #[allow(clippy::float_cmp)]
        let minify_filter = if scale == 1. {
            MinifySamplerFilter::Nearest
        } else {
            // TODO -- we'd want to rescale animated images if doing this, since linear, or even
            // linearmipmaplinear, causes fringing with transparent backgrounds.
            // MinifySamplerFilter::Nearest
            MinifySamplerFilter::Linear
        };

        let behaviour = SamplerBehavior {
            wrap_function: (
                SamplerWrapFunction::Repeat,
                SamplerWrapFunction::Repeat,
                SamplerWrapFunction::Repeat,
            ),
            magnify_filter: MagnifySamplerFilter::Nearest,
            minify_filter,
            max_anisotropy: 1,
            depth_texture_comparison: None,
        };


        let mut frame_draw = |tex: &Texture2d, matrix: [[f32; 4]; 4]| {
            let uniforms = uniform! {
                matrix: matrix,
                tex: Sampler(tex, behaviour)
            };

            frame
                .draw(
                    display.vertices.get().expect("Impossible"),
                    display.indices.get().expect("Impossible"),
                    display.program.get().expect("Impossible"),
                    &uniforms,
                    &DrawParameters {
                        blend: BLEND_PARAMS,
                        ..DrawParameters::default()
                    },
                )
                .unwrap();
        };

        // TODO -- figure out if this image is currently visible.
        let visible = match (
            is_visible_1d(0, current_res.w, scale, ofx, target_size.w),
            is_visible_1d(0, current_res.h, scale, ofy, target_size.h),
        ) {
            (Visibility::Visible, Visibility::Visible) => Visibility::Visible,
            (Visibility::UnloadTile, _) | (_, Visibility::UnloadTile) => Visibility::UnloadTile,
            (Visibility::Offscreen, _) | (_, Visibility::Offscreen) => Visibility::Offscreen,
        };

        match &mut self.texture {
            TextureLayout::Single { current, previous } => {
                if visible != Visibility::Visible {
                    // TODO -- could take the texture and pass it up to a higher level cache
                    println!("Skipped drawing entire offscreen image");
                    return;
                }


                let tex = match current {
                    Some(tex) => tex,
                    None => {
                        *current = Some(Self::upload_whole_image(
                            &self.image.bgra,
                            display,
                            previous.take(),
                        ));
                        current.as_ref().unwrap()
                    }
                };

                frame_draw(
                    tex,
                    Self::render_matrix(
                        target_size,
                        current_res.w as f32,
                        current_res.h as f32,
                        scale,
                        (ofx, ofy),
                    )
                    .into(),
                )
            }
            TextureLayout::Tiled { tiles, reuse_cache, .. } => {
                // TODO -- Track if any tiles are uploaded and clear them if so.
                if visible != Visibility::Visible {
                    println!("Skipped drawing entire offscreen tiled image");
                    return;
                }


                let matrix = Self::render_matrix(
                    target_size,
                    TILE_SIZE as f32,
                    TILE_SIZE as f32,
                    scale,
                    (ofx, ofy),
                );

                for (y, row) in tiles.iter_mut().enumerate() {
                    let tile_ofy = y as u32 * TILE_SIZE;

                    // Unload or skip unnecessary rows.
                    // Rows are unloaded if they're more than one full row away from the edge of the
                    // visible region.
                    match is_visible_1d(tile_ofy, TILE_SIZE, scale, ofy, target_size.h) {
                        Visibility::Visible => (),
                        Visibility::Offscreen => continue,
                        Visibility::UnloadTile => {
                            // This will often be wasted but is unlikely to be a substantial burden.
                            for t in row.iter_mut() {
                                if let Some(tex) = t.take() {
                                    reuse_cache.push(tex);
                                }
                            }
                            continue;
                        }
                    }

                    for (x, t) in row.iter_mut().enumerate() {
                        let tile_ofx = x as u32 * TILE_SIZE;

                        // Unload or skip unnecessary tiles.
                        // Tiles are unloaded if more than one complete tile away from the edge of
                        // the visible region.
                        match is_visible_1d(tile_ofx, TILE_SIZE, scale, ofx, target_size.w) {
                            Visibility::Visible => (),
                            Visibility::Offscreen => continue,
                            Visibility::UnloadTile => {
                                if let Some(tex) = t.take() {
                                    reuse_cache.push(tex);
                                }

                                continue;
                            }
                        }

                        let tex = match t {
                            Some(tex) => &*tex,
                            None => {
                                *t = Some(Self::upload_tile(
                                    &self.image.bgra,
                                    display,
                                    tile_ofx,
                                    tile_ofy,
                                    reuse_cache.pop(),
                                ));
                                t.as_ref().unwrap()
                            }
                        };

                        frame_draw(
                            tex,
                            (matrix
                                * Matrix4::from_translation(Vector3::new(
                                    x as f32 * 2.0,
                                    y as f32 * 2.0,
                                    0.0,
                                )))
                            .into(),
                        );
                    }
                }
            }
        }

        println!("drew: {:?}", start.elapsed());
    }
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
    index: usize,
    status: AnimationStatus,
}

impl Animation {
    pub fn new(a: &AnimatedImage, play: bool) -> Rc<RefCell<Self>> {
        let textures = a.frames().map(|(bgra, _)| {
            StaticImage::new(
                ScaledImage {
                    bgra: bgra.clone(),
                    original_res: bgra.res,
                },
                AllocatedTextures::Nothing,
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
                index: 0,
                status,
            })
        })
    }

    fn set_playing(rc: &Rc<RefCell<Self>>, play: bool) {
        let mut ac = rc.borrow_mut();

        match (play, &ac.status) {
            (false, AnimationStatus::Playing { target_time, .. }) => {
                let residual = target_time.saturating_duration_since(Instant::now());
                ac.status = AnimationStatus::Paused(residual);
            }
            (true, AnimationStatus::Paused(residual)) => {
                let weak = rc.downgrade();
                let target_time = Instant::now()
                    .checked_add(*residual)
                    .unwrap_or_else(|| Instant::now() + Duration::from_secs(1));
                let timeout_id =
                    ManuallyDrop::new(glib::timeout_add_local_once(*residual, move || {
                        Self::advance_animation(weak)
                    }));
                ac.status = AnimationStatus::Playing { target_time, timeout_id };
            }
            (true, AnimationStatus::Playing { .. }) | (false, AnimationStatus::Paused(..)) => (),
        }
    }

    fn advance_animation(weak: Weak<RefCell<Self>>) {
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

        GUI.with(|gui| gui.get().unwrap().canvas_2.queue_draw());

        *timeout_id = glib::timeout_add_local_once(
            target_time.saturating_duration_since(Instant::now()),
            move || Self::advance_animation(weak),
        );
    }

    fn preload_frame(weak: Weak<RefCell<Self>>, next: usize) {
        let ac = match weak.upgrade() {
            Some(ac) => ac,
            None => return,
        };
        let mut ab = ac.borrow_mut();

        trace!("Preloading frame {next}");

        GUI.with(|gui| {
            let display = gui.get().unwrap().canvas_2.inner().renderer.borrow();
            ab.textures[next].preload(display.as_ref().unwrap());
        });
    }

    pub(super) fn invalidate(&mut self) {
        for t in self.textures.iter_deduped_mut() {
            t.invalidate();
        }
    }

    pub(super) fn draw(
        rc: &Rc<RefCell<Self>>,
        display: &Renderer,
        frame: &mut Frame,
        layout: (i32, i32, Res),
        target_size: Res,
    ) {
        let mut ab = rc.borrow_mut();
        let ac = &mut *ab;

        ac.textures[ac.index].draw(display, frame, layout, target_size);

        // Only preload after a successful draw, otherwise GTK can break the texture if it's done
        // before the first draw or immediately after the context is destroyed.
        let weak = Rc::downgrade(rc);
        let next = (ac.index + 1) % ac.animated.frames().len();
        if ac.textures[next].needs_animation_preload() {
            glib::idle_add_local_once(move || Self::preload_frame(weak, next));
        }
    }
}

#[allow(clippy::large_enum_variant)]
#[derive(Debug, Default)]
pub(in super::super) enum Renderable {
    #[default]
    Nothing,
    Pending(Res),
    Image(StaticImage),
    Animation(Rc<RefCell<Animation>>),
}

impl Renderable {
    fn set_playing(&mut self, play: bool) {
        match self {
            Self::Animation(a) => Animation::set_playing(a, play),
            // Self::Video(v) => {
            //     let ms = v.media_stream().unwrap();
            //     match (play, ms.is_playing()) {
            //         (Some(false) | None, true) => ms.set_playing(false),
            //         (Some(true) | None, false) => ms.set_playing(true),
            //         (Some(true), true) | (Some(false), false) => (),
            //     }
            // }
            _ => {}
        }
    }

    pub(super) fn matches(&self, disp: &Displayable) -> bool {
        match (self, disp) {
            (Self::Image(si), Displayable::Image(di)) => &si.image == di,
            (Self::Animation(sa), Displayable::Animation(da)) => &sa.borrow().animated == da,
            //(Self::Video(_sv), Displayable::Video(_dv)) => {
            //    error!("Videos cannot be equal yet");
            //    false
            //}
            // (Self::Error(se), Displayable::Error(de)) => se.text().as_str() == de,
            (Self::Pending(sr), Displayable::Pending(dr)) => sr == dr,
            (Self::Nothing, Displayable::Nothing) => true,
            _ => false,
        }
    }

    fn invalidate(&mut self) {
        match self {
            Self::Nothing | Self::Pending(_) => (),
            Self::Image(i) => i.invalidate(),
            Self::Animation(a) => a.borrow_mut().invalidate(),
        }
    }

    pub(super) fn take_textures(&mut self) -> AllocatedTextures {
        // TODO -- something for animations
        match self {
            Self::Image(i) => i.take_textures(),
            Self::Nothing | Self::Pending(_) | Self::Animation(_) => AllocatedTextures::default(),
        }
    }
}


#[allow(clippy::large_enum_variant)]
#[derive(Debug)]
pub(in super::super) enum DisplayedContent {
    Single(Renderable),
    Multiple(
        // TODO -- Add some extra caching for fast path including idle_unload
        // old: Option<Renderable>,
        // visible: Vec<Renderable>,
        Vec<Renderable>,
    ),
}

impl Default for DisplayedContent {
    fn default() -> Self {
        Self::Single(Renderable::default())
    }
}

impl DisplayedContent {
    pub(in super::super) fn set_playing(&mut self, play: bool) {
        match self {
            Self::Single(r) => r.set_playing(play),
            Self::Multiple(visible) => {
                for v in visible {
                    v.set_playing(play)
                }
            }
        }
    }

    pub(super) fn invalidate(&mut self) {
        match self {
            Self::Single(r) => r.invalidate(),
            Self::Multiple(visible) => {
                for v in visible {
                    v.invalidate()
                }
            }
        }
    }
}
