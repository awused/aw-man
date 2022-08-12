use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use glium::backend::{Backend, Facade};
use glium::debug::DebugCallbackBehavior;
use glium::index::PrimitiveType;
use glium::{implement_vertex, program, Frame, IndexBuffer, Program, Surface, VertexBuffer};
use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{gdk, glib};
use once_cell::unsync::OnceCell;

use super::renderable::{DisplayedContent, PreloadTask, Preloadable, Renderable};
use crate::com::{Displayable, GuiContent};
use crate::gui::glium_area::renderable::AllocatedTextures;
use crate::gui::{Gui, GUI};

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
        1.055 * f32::powf(s, 1.0 / 2.4) - 0.055
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
    // GTK does something really screwy, so if we need to invalidate once we'll need to do it again
    // next draw.
    invalidated: bool,

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
            invalidated: false,
            displayed: DisplayedContent::default(),
            next_page: Preloadable::default(),
            preload_tasks: Vec::new(),
            preload_id: None,
        };

        let vertices = VertexBuffer::new(
            &&rend,
            &[
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
            ],
        )
        .unwrap();

        let rnd = &rend;
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
            glium::IndexBuffer::new(&&rend, PrimitiveType::TriangleStrip, &[1, 2, 0, 3]).unwrap();

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

                if let Some(p) = p {
                    if old.matches(p) {
                        // This is the page up case. Can at least skip re-uploading `old`
                        let textures = self.next_page.take_renderable().take_textures();
                        self.next_page = std::mem::take(old).into();
                        self.displayed = DC::Single(Renderable::new(c, textures, &self.gui));
                        return;
                    }
                }

                let old_t = old.take_textures();

                if self.next_page.matches(c) {
                    (DC::Single(self.next_page.take_renderable()), Preloadable::new(p, old_t))
                } else {
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
            (DC::Single(s), GC::Multiple { visible, .. }) => {
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
            (DC::Multiple(old), GC::Multiple { visible, .. }) => {
                // If there are textures to take, it'll be overwhelmingly likely to be from the
                // first or last elements.
                let mut old_textures = if !visible.iter().any(|v| old[0].matches(v)) {
                    old[0].take_textures()
                } else if old.len() > 1 && !visible.iter().any(|v| old[old.len() - 1].matches(v)) {
                    old.last_mut().unwrap().take_textures()
                } else {
                    AllocatedTextures::default()
                };

                let visible = visible
                    .iter()
                    .map(|d| {
                        take_old_renderable(d, old).unwrap_or_else(|| {
                            Renderable::new(d, std::mem::take(&mut old_textures), &self.gui)
                        })
                    })
                    .collect();
                (DC::Multiple(visible), Preloadable::Nothing)
            }
        };
    }

    fn drop_textures(&mut self) {
        self.displayed.invalidate();
        self.next_page.drop_textures();
    }

    fn invalidate(&mut self) {
        self.drop_textures();
        self.invalidated = true;
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

            match self.gui.first_content_paint.get() {
                None => {
                    self.gui.first_content_paint.set(()).unwrap();
                    info!("Completed first meaningful paint");
                }
                Some(_) => (),
            }
        }

        if self.invalidated {
            self.invalidated = false;
            self.drop_textures();
        } else if schedule_preload {
            let g = self.gui.clone();
            self.preload_id = Some(glib::idle_add_local_once(move || g.canvas.inner().preload()));
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
    fn realize(&self, widget: &Self::Type) {
        self.parent_realize(widget);

        if widget.error().is_some() {
            return;
        }

        // SAFETY: we know the GdkGLContext exists as we checked for errors above, and we haven't
        // done any operations on it which could lead to glium's state mismatch. (In theory, GTK
        // doesn't do any state-breaking operations on the context either.)
        //
        // We will also ensure glium's context does not outlive the GdkGLContext by destroying it in
        // `unrealize()`.
        let context = unsafe {
            glium::backend::Context::new(self.instance(), true, DebugCallbackBehavior::default())
        }
        .unwrap();

        *self.renderer.borrow_mut() = Some(Renderer::new(context, self.instance()));
    }

    fn unrealize(&self, widget: &Self::Type) {
        *self.renderer.borrow_mut() = None;

        self.parent_unrealize(widget);
    }
}

impl GLAreaImpl for GliumGLArea {
    fn render(&self, _gl_area: &Self::Type, _context: &gtk::gdk::GLContext) -> bool {
        self.renderer.borrow_mut().as_mut().unwrap().draw();

        true
    }
}

impl GliumGLArea {
    pub fn invalidate(&self) {
        self.renderer.borrow_mut().as_mut().unwrap().invalidate();
    }

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

    fn preload(&self) {
        self.renderer.borrow_mut().as_mut().unwrap().run_preloads();
    }
}
