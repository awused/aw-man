use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gl::types::GLenum;
use glium::backend::{Backend, Facade};
use glium::debug::DebugCallbackBehavior;
use glium::index::PrimitiveType;
use glium::{Frame, IndexBuffer, Program, Surface, VertexBuffer, implement_vertex, program};
use gtk::glib::Propagation;
use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{gdk, glib};
use once_cell::unsync::OnceCell;

use super::renderable::{DisplayedContent, PreloadTask, Preloadable, Renderable};
use crate::closing;
use crate::com::{Displayable, GuiContent};
use crate::gui::glium_area::renderable::AllocatedTextures;
use crate::gui::{GUI, Gui};

#[derive(Copy, Clone)]
pub(super) struct Vertex {
    position: [f32; 2],
    tex_coords: [f32; 2],
}

implement_vertex!(Vertex, position, tex_coords);

#[inline]
fn srgb_to_linear(s: f32) -> f32 {
    if s <= 0.04045 { s / 12.92 } else { f32::powf((s + 0.055) / 1.055, 2.4) }
}

#[inline]
fn linear_to_srgb(s: f32) -> f32 {
    if s <= 0.003_130_8 {
        s * 12.92
    } else {
        1.055f32.mul_add(f32::powf(s, 1.0 / 2.4), -0.055)
    }
}

pub(super) struct RenderContext {
    pub vertices: VertexBuffer<Vertex>,
    pub program: Program,
    pub indices: IndexBuffer<u8>,
    pub context: Rc<glium::backend::Context>,
    // The background as linear RGB with premultiplied alpha, for passing to shaders.
    pub bg: [f32; 4],
}

impl Facade for &RenderContext {
    fn get_context(&self) -> &Rc<glium::backend::Context> {
        &self.context
    }
}


pub(super) struct Renderer {
    backend: super::GliumArea,
    gui: Rc<Gui>,
    render_context: OnceCell<RenderContext>,
    // The background as sRGB with premultiplied alpha (in linear gamma), for clearing tiles.
    clear_bg: [f32; 4],
    // This is another reference to the same context in render_context.context
    // Really only necessary for initialization and to better compartmentalize this struct.
    context: Rc<glium::backend::Context>,

    displayed: DisplayedContent,
    // Something not currently visible to preload
    next_page: Preloadable,
    // Preload tasks for something inside `displayed`. The next frames of animation or images that
    // are almost visible and could become visible by the next draw call.
    preload_tasks: Vec<PreloadTask>,
    preload_id: Option<glib::SourceId>,
}

impl Drop for Renderer {
    fn drop(&mut self) {
        if let Some(p) = self.preload_id.take() {
            p.remove();
        }
    }
}

impl Facade for &Renderer {
    fn get_context(&self) -> &Rc<glium::backend::Context> {
        &self.context
    }
}

impl Renderer {
    fn new(context: Rc<glium::backend::Context>, backend: super::GliumArea) -> Self {
        let gui = GUI.with(|g| g.get().expect("Can't realize GliumArea without Gui").clone());

        let mut rend = Self {
            backend,
            gui: gui.clone(),
            render_context: OnceCell::new(),
            clear_bg: [0.0; 4],
            context: context.clone(),
            displayed: DisplayedContent::default(),
            next_page: Preloadable::default(),
            preload_tasks: Vec::new(),
            preload_id: None,
        };
        let rnd = &rend;

        let vertices = VertexBuffer::new(&rnd, &[
            Vertex {
                position: [-1.0, -1.0],
                tex_coords: [0.0, 0.0],
            },
            Vertex {
                position: [-1.0, 1.0],
                tex_coords: [0.0, 1.0],
            },
            Vertex {
                position: [1.0, 1.0],
                tex_coords: [1.0, 1.0],
            },
            Vertex {
                position: [1.0, -1.0],
                tex_coords: [1.0, 0.0],
            },
        ])
        .unwrap();

        let program = program!(&rnd,
        140 => {
            vertex: "
                #version 140
                uniform mat4 matrix;
                in vec2 position;
                in vec2 tex_coords;
                out vec2 v_tex_coords;
                void main() {
                    gl_Position = matrix * vec4(position, 0.0, 1.0);
                    v_tex_coords = tex_coords;
                }
            ",

            fragment: include_str!("fragment.glsl"),
        },)
        .unwrap();

        let indices =
            glium::IndexBuffer::new(&rnd, PrimitiveType::TriangleStrip, &[1, 2, 0, 3]).unwrap();

        assert!(
            rend.render_context
                .set(RenderContext {
                    context,
                    vertices,
                    program,
                    indices,
                    bg: Default::default(),
                })
                .is_ok()
        );

        rend.set_bg(gui.bg.get());

        unsafe {
            (&rend).get_context().exec_in_context(|| {
                gl::load_with(|symbol| rend.backend.get_proc_address(symbol).cast());
            });
        }

        rend
    }

    fn update_displayed(&mut self, content: &GuiContent) {
        use {DisplayedContent as DC, GuiContent as GC};

        if let Some(id) = self.preload_id.take() {
            id.remove();
        }


        let mut take_old_renderable = |d: &Displayable, old: &mut Vec<Renderable>| {
            if self.next_page.matches(d) {
                return Some(self.next_page.take_renderable());
            }

            for (i, o) in old.iter_mut().enumerate() {
                if o.matches(d) {
                    return Some(old.swap_remove(i));
                }
            }

            None
        };

        // This is complicated to maximize reuse of allocated textures.
        (self.displayed, self.next_page) = match (&mut self.displayed, &content) {
            (DC::Single(old), GC::Single { current: c, preload: p }) => {
                if old.matches(c) {
                    trace!("Old single content matches new displayable");
                    if let Some(pl) = p {
                        if self.next_page.matches(pl) {
                            // current and preload both match, no changes necessary
                            return;
                        }
                        // This should only really happen during rescaling
                    }

                    self.next_page =
                        Preloadable::new(p, self.next_page.take_renderable().take_textures());
                    return;
                }

                // TODO -- let chains
                if p.is_some() && old.matches(p.as_ref().unwrap()) {
                    // This is the page up case. Can at least skip re-uploading `old`
                    let textures = self.next_page.take_renderable().take_textures();
                    (
                        DC::Single(Renderable::new(c, textures, &self.gui)),
                        std::mem::take(old).into(),
                    )
                } else if self.next_page.matches(c) {
                    let old_t = old.take_textures();
                    (DC::Single(self.next_page.take_renderable()), Preloadable::new(p, old_t))
                } else {
                    let old_t = old.take_textures();
                    (
                        DC::Single(Renderable::new(c, old_t, &self.gui)),
                        Preloadable::new(p, self.next_page.take_renderable().take_textures()),
                    )
                }
            }
            (DC::Multiple(old), GC::Single { current: d, preload: p }) => {
                let r = take_old_renderable(d, old)
                    .unwrap_or_else(|| Renderable::new(d, old[0].take_textures(), &self.gui));
                (DC::Single(r), Preloadable::new(p, AllocatedTextures::Nothing))
            }
            (DC::Single(old), GC::Dual { visible, .. }) => {
                // Except in weird edge cases (user manually jumping +- 1 page) old will not match
                // visible.second().
                let first = if old.matches(visible.first()) {
                    std::mem::take(old)
                } else {
                    Renderable::new(visible.first(), old.take_textures(), &self.gui)
                };

                if let Some(second) = visible.second() {
                    let second = if self.next_page.matches(second) {
                        self.next_page.take_renderable()
                    } else {
                        let textures = self.next_page.take_renderable().take_textures();
                        Renderable::new(second, textures, &self.gui)
                    };

                    (DC::Multiple(vec![first, second]), Preloadable::Nothing)
                } else {
                    (DC::Single(first), Preloadable::Nothing)
                }
            }
            (DC::Multiple(old), GC::Dual { visible, .. }) => {
                let maybe_first = take_old_renderable(visible.first(), old);

                let second = if let Some(second) = visible.second() {
                    if let Some(r) = take_old_renderable(second, old) {
                        Some(r)
                    } else if let Some(mut r) = old.pop() {
                        Some(Renderable::new(second, r.take_textures(), &self.gui))
                    } else {
                        Some(Renderable::new(second, AllocatedTextures::Nothing, &self.gui))
                    }
                } else {
                    None
                };

                let first = if let Some(first) = maybe_first {
                    first
                } else if let Some(mut r) = old.pop() {
                    Renderable::new(visible.first(), r.take_textures(), &self.gui)
                } else {
                    Renderable::new(visible.first(), AllocatedTextures::Nothing, &self.gui)
                };

                if let Some(second) = second {
                    (DC::Multiple(vec![first, second]), Preloadable::Nothing)
                } else {
                    (DC::Single(first), Preloadable::Nothing)
                }
            }
            (DC::Single(s), GC::Strip { visible, .. }) => {
                let visible = visible
                    .iter()
                    .map(|d| {
                        if s.matches(d) {
                            std::mem::take(s)
                        } else {
                            Renderable::new(d, s.take_textures(), &self.gui)
                        }
                    })
                    .collect();
                (DC::Multiple(visible), Preloadable::Nothing)
            }
            (DC::Multiple(old), GC::Strip { visible, .. }) => {
                let visible: Vec<_> = visible
                    .iter()
                    .map(|d| {
                        // This is O(n^2) but the actual number of elements is going to be small.
                        // Could revisit later if absolutely necessary. This can be optimized due
                        // to the order always being preserved.
                        // Worst case is only when long jumping.
                        (d, take_old_renderable(d, old))
                    })
                    .collect();

                let visible = visible
                    .into_iter()
                    .map(|(d, v)| {
                        v.unwrap_or_else(|| {
                            let tex = old
                                .pop()
                                .map_or(AllocatedTextures::Nothing, |mut r| r.take_textures());
                            Renderable::new(d, tex, &self.gui)
                        })
                    })
                    .collect();

                (DC::Multiple(visible), Preloadable::Nothing)
            }
        };

        match &self.displayed {
            DC::Single(Renderable::Video(v)) => {
                self.gui.progress.borrow_mut().attach_video(v, &self.gui);
            }
            DC::Single(Renderable::Animation(a)) => {
                a.borrow().setup_progress(&mut self.gui.progress.borrow_mut());
            }
            DC::Single(_) | DC::Multiple(_) => {
                self.gui.progress.borrow_mut().hide();
            }
        }
    }

    fn drop_textures(&mut self) {
        self.displayed.drop_textures();
        self.next_page.drop_textures();
    }

    fn set_bg(&mut self, srgb_bg: gdk::RGBA) {
        let a = srgb_bg.alpha();
        let bg = [
            srgb_to_linear(srgb_bg.red()) * a,
            srgb_to_linear(srgb_bg.green()) * a,
            srgb_to_linear(srgb_bg.blue()) * a,
            a,
        ];
        self.render_context.get_mut().unwrap().bg = bg;
        self.clear_bg =
            [linear_to_srgb(bg[0]), linear_to_srgb(bg[1]), linear_to_srgb(bg[2]), bg[3]];
    }

    fn draw(&mut self) {
        let start = Instant::now();

        // Restore the texture/sampler binds that glium thinks exist
        unsafe {
            self.context.exec_with_context(|c| {
                for (i, t) in
                    c.state.texture_units.iter().enumerate().filter(|(_i, t)| t.texture != 0)
                {
                    gl::ActiveTexture(gl::TEXTURE0 + i as GLenum);

                    let mut id = 0;
                    // This is not a great assumption - it would be better to store the set of bind
                    // points for live textures, but I know all textures used by glium in this
                    // program are 2D.
                    gl::GetIntegerv(gl::TEXTURE_BINDING_2D, &mut id);
                    if id < 0 || id as u32 != t.texture {
                        gl::BindTexture(gl::TEXTURE_2D, t.texture);
                        gl::BindSampler(i as GLenum, t.sampler);
                    }
                }

                gl::ActiveTexture(gl::TEXTURE0 + c.state.active_texture);
            });
        }

        if let Some(p) = self.preload_id.take() {
            p.remove();
        }

        let context = self.context.clone();
        let (w, h) = context.get_framebuffer_dimensions();

        let mut frame = Frame::new(context, (w, h));
        let mut drew_something = false;
        let mut schedule_preload = self.next_page.needs_preload();
        self.preload_tasks.clear();


        let r_ctx = self.render_context.get().unwrap();

        frame.clear_color(self.clear_bg[0], self.clear_bg[1], self.clear_bg[2], self.clear_bg[3]);

        {
            let layout_manager = self.gui.layout_manager.borrow();
            let mut layouts = layout_manager.layout_iter();


            let mut render = |d: &mut Renderable| {
                let layout = layouts.next().expect("Layout not defined for all displayed pages.");
                let (drew, preload) = d.draw(r_ctx, &mut frame, layout, (w, h).into());
                drew_something |= drew;
                schedule_preload |= preload != PreloadTask::Nothing;
                self.preload_tasks.push(preload);
            };

            match &mut self.displayed {
                DisplayedContent::Single(r) => render(r),
                DisplayedContent::Multiple(visible) => visible.iter_mut().for_each(render),
            }
        }

        frame.finish().unwrap();
        if drew_something {
            let last_action = self.gui.last_action.take();
            if let Some(last_action) = last_action {
                let dur = last_action.elapsed();

                if dur < Duration::from_millis(10) {
                    trace!("Took {dur:?} from action to drawable change.");
                } else if dur < Duration::from_millis(100) {
                    debug!("Took {dur:?} from action to drawable change.");
                } else if dur < Duration::from_secs(10) {
                    info!("Took {dur:?} from action to drawable change.");
                }
                // More than 10 seconds probably wasn't an action that changed anything.
            }

            let dur = start.elapsed();

            if dur < Duration::from_millis(10) {
                // Don't bother
            } else if dur < Duration::from_millis(20) {
                debug!("Took {dur:?} to draw frame.");
            } else if dur < Duration::from_millis(30) {
                info!("Took {dur:?} to draw frame.");
            } else {
                warn!("Took {dur:?} to draw frame.");
            }

            if !self.gui.first_content_paint.get() {
                self.gui.first_content_paint.set(true);
                info!("Completed first meaningful paint");
            }
        }

        if schedule_preload {
            let g = self.gui.clone();

            self.preload_id = Some(glib::idle_add_local_full(glib::Priority::LOW, move || {
                g.canvas.inner().preload();
                glib::ControlFlow::Break
            }));
        }
    }

    fn run_preloads(&mut self) {
        let ctx = self.render_context.get().unwrap();
        match &mut self.displayed {
            DisplayedContent::Single(r) => r.preload(ctx, self.preload_tasks.remove(0)),
            DisplayedContent::Multiple(visible) => {
                for (r, t) in visible.iter_mut().zip(self.preload_tasks.drain(..)) {
                    r.preload(ctx, t)
                }
            }
        }

        self.next_page.preload(ctx);
        self.preload_id = None;
    }
}

#[derive(Default)]
pub struct GliumGLArea {
    pub(super) renderer: RefCell<Option<Renderer>>,
}

#[glib::object_subclass]
impl ObjectSubclass for GliumGLArea {
    type ParentType = gtk::GLArea;
    type Type = super::GliumArea;

    const NAME: &'static str = "GliumGLArea";
}

impl ObjectImpl for GliumGLArea {}

impl WidgetImpl for GliumGLArea {
    fn realize(&self) {
        self.parent_realize();

        if let Some(e) = self.obj().error() {
            closing::fatal(format!("Error realizing opengl widget: {e}"));
            return;
        }

        // SAFETY: we know the GdkGLContext exists as we checked for errors above, and we haven't
        // done any operations on it which could lead to glium's state mismatch. (In theory, GTK
        // doesn't do any state-breaking operations on the context either.)
        //
        // We will also ensure glium's context does not outlive the GdkGLContext by destroying it in
        // `unrealize()`.
        let context = unsafe {
            glium::backend::Context::new(self.obj().clone(), true, DebugCallbackBehavior::default())
        }
        .unwrap();

        *self.renderer.borrow_mut() = Some(Renderer::new(context, self.obj().clone()));
    }

    fn unrealize(&self) {
        *self.renderer.borrow_mut() = None;

        self.parent_unrealize();
    }
}

impl GLAreaImpl for GliumGLArea {
    fn render(&self, _context: &gtk::gdk::GLContext) -> Propagation {
        self.renderer.borrow_mut().as_mut().unwrap().draw();

        Propagation::Stop
    }
}

impl GliumGLArea {
    pub fn idle_unload(&self) {
        trace!("Dropping all textures.");
        self.renderer.borrow_mut().as_mut().unwrap().drop_textures();
    }

    pub fn update_displayed(&self, content: &GuiContent) {
        self.renderer.borrow_mut().as_mut().unwrap().update_displayed(content);
    }

    pub fn set_bg(&self, bg: gdk::RGBA) {
        self.renderer.borrow_mut().as_mut().unwrap().set_bg(bg);
    }

    pub fn set_playing(&self, play: bool) {
        self.renderer.borrow_mut().as_mut().unwrap().displayed.set_playing(play);
    }

    pub fn seek_animation(&self, dur: Duration) {
        self.renderer.borrow().as_ref().unwrap().displayed.seek_animation(dur);
    }

    fn preload(&self) {
        self.renderer.borrow_mut().as_mut().unwrap().run_preloads();
    }
}
