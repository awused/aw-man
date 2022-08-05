use std::cmp::min;
use std::mem::ManuallyDrop;
use std::time::Instant;

use cgmath::{Matrix4, Ortho, Vector3};
use derive_more::Deref;
use glium::backend::Facade;
use glium::texture::{MipmapsOption, SrgbTexture2d};
use glium::uniforms::{
    MagnifySamplerFilter, MinifySamplerFilter, Sampler, SamplerBehavior, SamplerWrapFunction,
};
use glium::{uniform, Blend, BlendingFunction, DrawParameters, Frame, GlObject, Surface};
use gtk::glib;

use super::PreloadTask;
use crate::closing;
use crate::com::{Image, ImageWithRes, Res};
use crate::gui::glium_area::imp::RenderContext;
use crate::gui::layout::APPROX_SCROLL_STEP;

static TILE_SIZE: u32 = 512;
static MAX_UNTILED_SIZE: u32 = 8192;
// The minimum scale size we allow at render time. Set to avoid uploading extraordinarily large
// images in their entirety before they've had a chance to be downscaled by the CPU.
static MIN_AUTO_ZOOM: f32 = 0.2;

static BLEND_PARAMS: Blend = Blend {
    color: BlendingFunction::AlwaysReplace,
    alpha: BlendingFunction::AlwaysReplace,
    constant_value: (0.0, 0.0, 0.0, 0.0),
};

#[derive(Debug, Deref)]
pub struct Texture(ManuallyDrop<SrgbTexture2d>);

impl Drop for Texture {
    fn drop(&mut self) {
        // Safe because we're dropping the texture.
        let t = unsafe { ManuallyDrop::take(&mut self.0) };
        glib::idle_add_local_once(move || {
            if closing::closed() {
                std::mem::forget(t);
            } else {
                drop(t);
            }
        });
    }
}

impl From<SrgbTexture2d> for Texture {
    fn from(t: SrgbTexture2d) -> Self {
        Self(ManuallyDrop::new(t))
    }
}

#[derive(Default, Debug)]
pub enum AllocatedTextures {
    #[default]
    Nothing,
    Single(Texture),
    Tiles(Vec<Texture>),
}

#[derive(Debug, Default)]
enum SingleTexture {
    #[default]
    Nothing,
    Current(Texture),
    Allocated(Texture),
}

impl SingleTexture {
    fn take_texture(&mut self) -> Option<Texture> {
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
        tiles: Vec<Vec<Option<Texture>>>,
        // TODO -- reuse these between textures
        // TODO -- unload these during idle_unload
        reuse_cache: Vec<Texture>,
    },
}

#[derive(Debug)]
pub struct StaticImage {
    texture: TextureLayout,
    pub(super) image: ImageWithRes,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Visibility {
    Complete,
    Partial,
    // The image is offscreen but very close to being visible, as in within one scroll step at
    // 60hz.
    Preload,
    Offscreen,
    // If it's more than one full tile offscreen, it can be unloaded
    Unload,
}

impl Visibility {
    fn combine(self, b: Self) -> Self {
        use Visibility::*;
        match (self, b) {
            (Offscreen, _) | (_, Offscreen) => Offscreen,
            (Unload, _) | (_, Unload) => Unload,
            (Preload, _) | (_, Preload) => Preload,
            (Partial, _) | (_, Partial) => Partial,
            (Complete, Complete) => Complete,
        }
    }
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
    let real_start = (inset as f32 * scale).round() as i32 + offset;
    let real_end = ((inset + dimension) as f32 * scale).round() as i32 + offset - 1;
    let real_tile = (TILE_SIZE as f32 * scale).ceil() as i32;
    let display_end = target_dim as i32 - 1;

    if real_start - real_tile > display_end || real_end + real_tile < 0 {
        Visibility::Unload
    } else if real_start - *APPROX_SCROLL_STEP > display_end || real_end + *APPROX_SCROLL_STEP < 0 {
        Visibility::Offscreen
    } else if real_start > display_end || real_end < 0 {
        Visibility::Preload
    } else if real_start >= 0 && real_end <= display_end {
        Visibility::Complete
    } else {
        Visibility::Partial
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

    fn upload_whole_image(img: &Image, ctx: &RenderContext, existing: Option<Texture>) -> Texture {
        let start = Instant::now();
        let w = img.res.w;
        let h = img.res.h;

        let tex = match existing {
            Some(tex) if tex.width() == w && tex.height() == h => tex,
            _ => SrgbTexture2d::empty_with_mipmaps(&ctx, MipmapsOption::NoMipmap, w, h)
                .unwrap()
                .into(),
        };


        let g_layout = img.gl_layout();

        unsafe {
            ctx.get_context().exec_in_context(|| {
                gl::PixelStorei(gl::UNPACK_ALIGNMENT, g_layout.alignment);

                gl::TextureParameteriv(
                    tex.get_id(),
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
        existing: Option<Texture>,
    ) -> Texture {
        let start = Instant::now();
        let width = min(TILE_SIZE, img.res.w - x);
        let height = min(TILE_SIZE, img.res.h - y);

        // Fixed size tiles, at least for now
        let tex = existing.unwrap_or_else(|| {
            SrgbTexture2d::empty_with_mipmaps(&ctx, MipmapsOption::NoMipmap, TILE_SIZE, TILE_SIZE)
                .unwrap()
                .into()
        });

        let g_layout = img.gl_layout();

        unsafe {
            ctx.get_context().exec_in_context(|| {
                gl::PixelStorei(gl::UNPACK_ROW_LENGTH, img.res.w as i32);
                gl::PixelStorei(gl::UNPACK_ALIGNMENT, g_layout.alignment);

                static TILE_BG: [u8; 4] = [0u8, 0, 0, 0];

                gl::TextureParameteriv(
                    tex.get_id(),
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

    pub(super) fn needs_preload(&self) -> bool {
        match &self.texture {
            TextureLayout::Single(SingleTexture::Current(_)) => false,
            TextureLayout::Single(_) => true,
            TextureLayout::Tiled { any_uploaded, reuse_cache, .. } => {
                !any_uploaded && reuse_cache.is_empty()
            }
        }
    }

    pub(super) fn attach_textures(&mut self, allocated: AllocatedTextures) {
        use {SingleTexture as ST, TextureLayout as TL};

        match (&mut self.texture, allocated) {
            (TL::Single(ST::Nothing), AllocatedTextures::Single(s)) => {
                self.texture = TL::Single(ST::Allocated(s));
            }
            (TL::Tiled { any_uploaded, reuse_cache, .. }, AllocatedTextures::Tiles(tiles))
                if !*any_uploaded && reuse_cache.is_empty() =>
            {
                *reuse_cache = tiles;
            }
            (..) => {}
        }
    }

    // Preloads the frame of animation. For tiled frames this only attaches the tile reuse cache.
    pub(super) fn preload(&mut self, ctx: &RenderContext, load_tiles: Vec<(usize, usize)>) {
        use {SingleTexture as ST, TextureLayout as TL};

        match &mut self.texture {
            TL::Single(ST::Current(_)) => {}
            TL::Single(st) => {
                *st = ST::Current(Self::upload_whole_image(&self.image.img, ctx, st.take_texture()))
            }
            TL::Tiled { any_uploaded, reuse_cache, tiles, .. } => {
                *any_uploaded |= !load_tiles.is_empty();

                for (x, y) in load_tiles {
                    tiles[y][x] = Some(Self::upload_tile(
                        &self.image.img,
                        ctx,
                        x as u32 * TILE_SIZE,
                        y as u32 * TILE_SIZE,
                        reuse_cache.pop(),
                    ));
                }
            }
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
    ) -> (bool, PreloadTask) {
        let (ofx, ofy, res) = layout;
        if res.is_zero_area() {
            warn!("Attempted to draw 0 sized image");
            return (false, PreloadTask::Nothing);
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
            return (false, PreloadTask::Nothing);
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


        let mut frame_draw = |tex: &Texture, matrix: [[f32; 4]; 4]| {
            let uniforms = uniform! {
                matrix: matrix,
                tex: Sampler(&***tex, behaviour),
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
        let visible = is_visible_1d(0, current_res.w, scale, ofx, target_size.w)
            .combine(is_visible_1d(0, current_res.h, scale, ofy, target_size.h));


        use {SingleTexture as ST, TextureLayout as TL};

        match (visible, &mut self.texture) {
            (Complete | Partial, TL::Single(ST::Current(_))) => (),
            (Complete | Partial, TL::Single(st)) => {
                *st =
                    ST::Current(Self::upload_whole_image(&self.image.img, ctx, st.take_texture()));
            }
            (Complete, TL::Tiled { .. }) => {
                // Tiling only makes sense if we're not drawing the entire image.
                // This should only happen very rarely, like on initial load or when changing
                // settings. This should still be limited to almost reasonable values by
                // MIN_AUTO_ZOOM and the requirement for complete visibility.
                warn!("Overriding maximum tiled image size for entirely visible image.");
                self.texture =
                    TL::Single(ST::Current(Self::upload_whole_image(&self.image.img, ctx, None)));
            }
            (Partial, TL::Tiled { any_uploaded, .. }) => *any_uploaded = true,
            (Preload, TL::Single(ST::Nothing | ST::Allocated(_))) => {
                return (false, PreloadTask::WholeImage);
            }
            (Preload, TL::Tiled { .. }) => {}
            (Preload | Offscreen, _) => {
                // While there might be tiles we can unrender it's probably not worth the effort.
                return (false, PreloadTask::Nothing);
            }
            (Unload, _) => {
                // TODO -- could take textures in the future.
                self.invalidate();
                return (false, PreloadTask::Nothing);
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
                let mut preload_tiles = Vec::new();

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
                    let row_visibility =
                        is_visible_1d(tile_ofy, TILE_SIZE, scale, ofy, target_size.h);
                    match row_visibility {
                        Complete | Partial | Preload => (),
                        Offscreen => continue,
                        Unload => {
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
                        let col_visibility =
                            is_visible_1d(tile_ofx, TILE_SIZE, scale, ofx, target_size.w);
                        let tile_visibility = row_visibility.combine(col_visibility);
                        match tile_visibility {
                            Complete | Partial => (),
                            Preload if t.is_none() => {
                                preload_tiles.push((x, y));
                                continue;
                            }
                            Preload | Offscreen => continue,
                            Unload => {
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

                if !preload_tiles.is_empty() {
                    return (true, PreloadTask::Tiles(preload_tiles));
                }
            }
        }

        (true, PreloadTask::Nothing)
    }
}
