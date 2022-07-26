use std::cell::RefCell;
use std::cmp::min;
use std::mem::ManuallyDrop;
use std::rc::{Rc, Weak};
use std::time::{Duration, Instant};

use cgmath::{Matrix4, Ortho, Vector3};
use glium::backend::Facade;
use glium::texture::{MipmapsOption, SrgbTexture2d};
use glium::uniforms::{
    MagnifySamplerFilter, MinifySamplerFilter, Sampler, SamplerBehavior, SamplerWrapFunction,
};
use glium::{uniform, Blend, BlendingFunction, DrawParameters, Frame, GlObject, Surface};
use gtk::glib::{self, SourceId};
use gtk::prelude::*;
use gtk::traits::WidgetExt;

use super::imp::RenderContext;
use crate::com::{AnimatedImage, DedupedVec, Displayable, Image, ImageWithRes, Res};
use crate::gui::GUI;

static TILE_SIZE: u32 = 512;
static MAX_UNTILED_SIZE: u32 = 8192;
// static MAX_UNTILED_SIZE: u32 = 16384;
// The minimum scale size we allow at render time. Set to avoid uploading extraordinarily large
// images in their entirety before they've had a chance to be downscaled by the CPU.
static MIN_AUTO_ZOOM: f32 = 0.2;

static BLEND_PARAMS: Blend = Blend {
    color: BlendingFunction::AlwaysReplace,
    alpha: BlendingFunction::AlwaysReplace,
    constant_value: (0.0, 0.0, 0.0, 0.0),
};


#[derive(Default, Debug)]
pub enum AllocatedTextures {
    #[default]
    Nothing,
    Single(SrgbTexture2d),
    Tiles(Vec<SrgbTexture2d>),
}

#[derive(Debug, Default)]
enum SingleTexture {
    #[default]
    Nothing,
    Current(SrgbTexture2d),
    Allocated(SrgbTexture2d),
}

impl SingleTexture {
    fn take_texture(&mut self) -> Option<SrgbTexture2d> {
        let old = std::mem::take(self);
        match old {
            Self::Current(t) | Self::Allocated(t) => Some(t),
            Self::Nothing => None,
        }
    }
}

#[derive(Debug)]
enum TextureLayout {
    Single(SingleTexture),
    Tiled {
        columns: f32,
        rows: f32,
        any_uploaded: bool,
        tiles: Vec<Vec<Option<SrgbTexture2d>>>,
        // TODO -- reuse these between textures
        // TODO -- unload these during idle_unload
        reuse_cache: Vec<SrgbTexture2d>,
    },
}

#[derive(Debug)]
pub struct StaticImage {
    texture: TextureLayout,
    image: ImageWithRes,
}

#[derive(Debug, PartialEq, Eq)]
enum Visibility {
    Visible,
    Offscreen,
    // If it's more than one full tile offscreen, it can be unloaded
    UnloadTile,
}

fn is_visible_1d(
    // inset and dimension are in image coordinates
    inset: u32,
    dimension: u32,
    scale: f32,
    // offset and target_dim are in display coordinates
    offset: i32,
    target_dim: u32,
) -> Visibility {
    // Subtract 1 from ending values to use inclusive coordinates on both ends.
    let real_start = (inset as f32 * scale).floor() as i32 + offset;
    let real_end = ((inset + dimension) as f32 * scale).ceil() as i32 + offset - 1;
    let real_tile = (TILE_SIZE as f32 * scale).ceil() as i32;


    if real_start - real_tile > target_dim as i32 - 1 || real_end + real_tile < 0 {
        Visibility::UnloadTile
    } else if real_start > target_dim as i32 - 1 || real_end < 0 {
        Visibility::Offscreen
    } else {
        Visibility::Visible
    }
}

impl StaticImage {
    pub fn new(image: ImageWithRes, allocated: AllocatedTextures) -> Self {
        let c_res = image.img.res;
        let texture = if c_res.h <= MAX_UNTILED_SIZE && c_res.w <= MAX_UNTILED_SIZE {
            if let AllocatedTextures::Single(tex) = allocated {
                TextureLayout::Single(SingleTexture::Allocated(tex))
            } else {
                TextureLayout::Single(SingleTexture::default())
            }
        } else {
            let rows = c_res.h as f32 / TILE_SIZE as f32;
            let columns = c_res.w as f32 / TILE_SIZE as f32;

            let mut tiles = Vec::with_capacity(rows.ceil() as usize);
            tiles.resize_with(tiles.capacity(), || {
                let mut row = Vec::with_capacity(columns.ceil() as usize);
                row.resize_with(row.capacity(), || None);
                row
            });

            let reuse_cache =
                if let AllocatedTextures::Tiles(tiles) = allocated { tiles } else { Vec::new() };

            TextureLayout::Tiled {
                tiles,
                any_uploaded: false,
                columns,
                rows,
                reuse_cache,
            }
        };


        Self { texture, image }
    }

    fn upload_whole_image(
        img: &Image,
        ctx: &RenderContext,
        existing: Option<SrgbTexture2d>,
    ) -> SrgbTexture2d {
        let start = Instant::now();
        let w = img.res.w;
        let h = img.res.h;

        let tex = match existing {
            Some(tex) if tex.width() == w && tex.height() == h => tex,
            _ => SrgbTexture2d::empty_with_mipmaps(&ctx, MipmapsOption::NoMipmap, w, h).unwrap(),
        };

        let g_layout = img.gl_layout();

        unsafe {
            ctx.get_context().exec_in_context(|| {
                gl::PixelStorei(gl::UNPACK_ALIGNMENT, g_layout.alignment);

                gl::TexParameteriv(
                    gl::TEXTURE_2D,
                    gl::TEXTURE_SWIZZLE_RGBA,
                    std::ptr::addr_of!(g_layout.swizzle) as _,
                );

                gl::TextureSubImage2D(
                    tex.get_id(),
                    0,
                    0,
                    0,
                    w as i32,
                    h as i32,
                    g_layout.format,
                    gl::UNSIGNED_BYTE,
                    img.as_ptr() as *const libc::c_void,
                );

                gl::PixelStorei(gl::UNPACK_ALIGNMENT, 4);
            });
        }
        trace!("Uploaded whole image: {:?}", start.elapsed());
        tex
    }

    fn upload_tile(
        img: &Image,
        ctx: &RenderContext,
        x: u32,
        y: u32,
        existing: Option<SrgbTexture2d>,
    ) -> SrgbTexture2d {
        let start = Instant::now();
        let width = min(TILE_SIZE, img.res.w - x);
        let height = min(TILE_SIZE, img.res.h - y);

        // Fixed size tiles, at least for now
        let tex = existing.unwrap_or_else(|| {
            SrgbTexture2d::empty_with_mipmaps(&ctx, MipmapsOption::NoMipmap, TILE_SIZE, TILE_SIZE)
                .unwrap()
        });

        let g_layout = img.gl_layout();

        unsafe {
            ctx.get_context().exec_in_context(|| {
                gl::PixelStorei(gl::UNPACK_ROW_LENGTH, img.res.w as i32);
                gl::PixelStorei(gl::UNPACK_ALIGNMENT, g_layout.alignment);

                // TODO -- revert to 0, 0, 0, 0
                static TILE_BG: [u8; 4] = [0u8, 0xff, 0, 0xff];

                gl::TexParameteriv(
                    gl::TEXTURE_2D,
                    gl::TEXTURE_SWIZZLE_RGBA,
                    std::ptr::addr_of!(g_layout.swizzle) as _,
                );

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
                    g_layout.format,
                    gl::UNSIGNED_BYTE,
                    img.as_offset_ptr(x, y) as *const libc::c_void,
                );

                gl::PixelStorei(gl::UNPACK_ALIGNMENT, 4);
                gl::PixelStorei(gl::UNPACK_ROW_LENGTH, 0);
            });
        }
        trace!("Uploaded tile: {:?}", start.elapsed());
        tex
    }

    fn needs_animation_preload(&self) -> bool {
        match &self.texture {
            TextureLayout::Single(SingleTexture::Current(_)) => false,
            TextureLayout::Single(_) => true,
            TextureLayout::Tiled { any_uploaded, reuse_cache, .. } => {
                !any_uploaded && reuse_cache.is_empty()
            }
        }
    }

    // Preloads the frame of animation. For tiled frames this only attaches the tile reuse cache.
    fn preload(&mut self, ctx: &RenderContext, allocated: AllocatedTextures) {
        use {SingleTexture as ST, TextureLayout as TL};

        match (&mut self.texture, allocated) {
            (TL::Single(st @ ST::Allocated(_)), _) => {
                *st = ST::Current(Self::upload_whole_image(&self.image.img, ctx, st.take_texture()))
            }
            (TL::Single(st @ ST::Nothing), AllocatedTextures::Single(s)) => {
                *st = ST::Current(Self::upload_whole_image(&self.image.img, ctx, Some(s)))
            }
            (TL::Single(st @ ST::Nothing), _) => {
                *st = ST::Current(Self::upload_whole_image(&self.image.img, ctx, None))
            }
            (TL::Tiled { any_uploaded, reuse_cache, .. }, _)
                if *any_uploaded || !reuse_cache.is_empty() => {}
            (TL::Tiled { reuse_cache, .. }, AllocatedTextures::Tiles(tiles)) => {
                *reuse_cache = tiles;
            }
            (TL::Tiled { .. } | TL::Single(ST::Current(_)), _) => {}
        }
    }

    pub fn take_textures(&mut self) -> AllocatedTextures {
        // Take a texture for reuse for the next image, if possible.
        // Currently only for images below the cutoff size for single tiles.
        match &mut self.texture {
            TextureLayout::Single(st) => {
                st.take_texture().map_or(AllocatedTextures::Nothing, AllocatedTextures::Single)
            }
            TextureLayout::Tiled { tiles, reuse_cache, any_uploaded, .. }
                if *any_uploaded || !reuse_cache.is_empty() =>
            {
                let mut reuse_cache = std::mem::take(reuse_cache);

                *any_uploaded = false;
                for t in tiles.iter_mut().flatten().filter_map(Option::take) {
                    reuse_cache.push(t);
                }

                AllocatedTextures::Tiles(reuse_cache)
            }
            TextureLayout::Tiled { .. } => AllocatedTextures::Nothing,
        }
    }

    pub fn invalidate(&mut self) {
        match &mut self.texture {
            TextureLayout::Single(t) => {
                *t = SingleTexture::Nothing;
            }
            TextureLayout::Tiled {
                tiles,
                reuse_cache,
                any_uploaded,
                columns: _columns,
                rows: _rows,
            } => {
                if *any_uploaded {
                    for row in tiles {
                        for t in row {
                            *t = None;
                        }
                    }
                    *any_uploaded = false;
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
        ctx: &RenderContext,
        frame: &mut Frame,
        layout: (i32, i32, Res),
        target_size: Res,
    ) -> bool {
        let (ofx, ofy, res) = layout;
        if res.is_zero_area() {
            warn!("Attempted to draw 0 sized image");
            return false;
        }

        let current_res = self.image.img.res;

        let scale = if res.w == current_res.w {
            1.
        } else {
            debug!("Needed to scale image at draw time. {:?} -> {:?}", current_res, res);
            res.w as f32 / current_res.w as f32
        };

        if scale < MIN_AUTO_ZOOM {
            warn!(
                "Skipping rendering since scale ({scale:?}) was too low (minimum: \
                 {MIN_AUTO_ZOOM:?}). Waiting for downscaling."
            );
            return false;
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


        let mut frame_draw = |tex: &SrgbTexture2d, matrix: [[f32; 4]; 4]| {
            let uniforms = uniform! {
                matrix: matrix,
                tex: Sampler(tex, behaviour),
                bg: ctx.bg,
                grey: self.image.img.grey(),
            };


            frame
                .draw(
                    &ctx.vertices,
                    &ctx.indices,
                    &ctx.program,
                    &uniforms,
                    &DrawParameters {
                        blend: BLEND_PARAMS,
                        ..DrawParameters::default()
                    },
                )
                .unwrap();
        };

        use Visibility::*;
        let visible = match (
            is_visible_1d(0, current_res.w, scale, ofx, target_size.w),
            is_visible_1d(0, current_res.h, scale, ofy, target_size.h),
        ) {
            (Visible, Visible) => Visible,
            (UnloadTile, _) | (_, UnloadTile) => UnloadTile,
            (Offscreen, _) | (_, Offscreen) => Offscreen,
        };

        use {SingleTexture as ST, TextureLayout as TL};

        match (visible, &mut self.texture) {
            (Visible, TL::Single(ST::Current(_))) => (),
            (Visible, TL::Single(st)) => {
                *st = ST::Current(Self::upload_whole_image(&self.image.img, ctx, st.take_texture()))
            }
            (Visible, TL::Tiled { any_uploaded, .. }) => *any_uploaded = true,
            (Offscreen, _) => {
                // While there might be tiles we can unrender it's probably not worth the effort.
                return false;
            }
            (UnloadTile, _) => {
                // TODO -- could take textures in the future.
                self.invalidate();
                return false;
            }
        }

        match &mut self.texture {
            TL::Single(ST::Current(tex)) => frame_draw(
                tex,
                Self::render_matrix(
                    target_size,
                    current_res.w as f32,
                    current_res.h as f32,
                    scale,
                    (ofx, ofy),
                )
                .into(),
            ),
            TL::Single(_) => unreachable!(),
            TL::Tiled { tiles, reuse_cache, .. } => {
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

                        let tex = &*t.get_or_insert_with(|| {
                            Self::upload_tile(
                                &self.image.img,
                                ctx,
                                tile_ofx,
                                tile_ofy,
                                reuse_cache.pop(),
                            )
                        });

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

        true
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
                ImageWithRes { img: img.clone(), original_res: img.res },
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

    fn set_playing(rc: &Rc<RefCell<Self>>, play: bool) {
        let mut ac = rc.borrow_mut();

        match (play, &ac.status) {
            (false, AnimationStatus::Playing { target_time, .. }) => {
                let residual = target_time.saturating_duration_since(Instant::now());
                ac.status = AnimationStatus::Paused(residual);
            }
            (true, AnimationStatus::Paused(residual)) => {
                let weak = Rc::downgrade(rc);
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

        GUI.with(|gui| gui.get().unwrap().canvas.queue_draw());

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
        let mut aref = ac.borrow_mut();
        let ab = &mut *aref;

        GUI.with(|gui| {
            let renderer = gui.get().unwrap().canvas.inner().renderer.borrow();
            let r_context = renderer.as_ref().unwrap().render_context.get().unwrap();
            ab.textures[next].preload(r_context, std::mem::take(&mut ab.preload_textures));
        });
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
        rc: &Rc<RefCell<Self>>,
        ctx: &RenderContext,
        frame: &mut Frame,
        layout: (i32, i32, Res),
        target_size: Res,
    ) {
        let mut ab = rc.borrow_mut();
        let ac = &mut *ab;

        let drew = ac.textures[ac.index].draw(ctx, frame, layout, target_size);

        if !drew {
            return;
        }

        // Only preload after a successful draw, otherwise GTK can break the texture if it's done
        // before the first draw or immediately after the context is destroyed.
        let weak = Rc::downgrade(rc);
        let next = (ac.index + 1) % ac.animated.frames().len();
        if ac.textures[next].needs_animation_preload() {
            let prev = if ac.index == 0 { ac.animated.frames().len() - 1 } else { ac.index - 1 };

            if prev != next && prev != ac.index {
                ac.preload_textures = ac.textures[prev].take_textures();
            }

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
    Video(gtk::Video),
    Error(gtk::Label),
}

impl Drop for Renderable {
    fn drop(&mut self) {
        match self {
            Self::Video(vid) => vid
                .parent()
                .unwrap()
                .dynamic_cast::<gtk::Overlay>()
                .unwrap()
                .remove_overlay(vid),
            Self::Error(e) => {
                e.parent().unwrap().dynamic_cast::<gtk::Overlay>().unwrap().remove_overlay(e)
            }
            _ => (),
        }
    }
}

impl Renderable {
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
            Self::Image(_) | Self::Pending(_) | Self::Error(_) | Self::Nothing => {}
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
            (Self::Pending(sr), Displayable::Pending(dr)) => sr == dr,
            (Self::Nothing, Displayable::Nothing) => true,
            (
                Self::Image(_)
                | Self::Animation(_)
                | Self::Video(_)
                | Self::Error(_)
                | Self::Pending(_)
                | Self::Nothing,
                _,
            ) => false,
        }
    }

    fn invalidate(&mut self) {
        match self {
            Self::Nothing | Self::Pending(_) | Self::Video(_) | Self::Error(_) => (),
            Self::Image(i) => i.invalidate(),
            Self::Animation(a) => a.borrow_mut().invalidate(),
        }
    }

    pub(super) fn take_textures(&mut self) -> AllocatedTextures {
        match self {
            Self::Image(i) => i.take_textures(),
            Self::Animation(a) => a.borrow_mut().take_textures(),
            Self::Nothing | Self::Pending(_) | Self::Video(_) | Self::Error(_) => {
                AllocatedTextures::default()
            }
        }
    }
}


#[allow(clippy::large_enum_variant)]
#[derive(Debug)]
pub(in super::super) enum DisplayedContent {
    Single(Renderable),
    Multiple(Vec<Renderable>),
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
