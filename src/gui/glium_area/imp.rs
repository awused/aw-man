use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use glium::backend::{Backend, Facade};
use glium::debug::DebugCallbackBehavior;
use glium::index::PrimitiveType;
use glium::{implement_vertex, program, Frame, IndexBuffer, Program, Surface, VertexBuffer};
use gtk::glib;
use gtk::prelude::*;
use gtk::subclass::prelude::*;
use once_cell::unsync::OnceCell;

use super::renderable::{Animation, DisplayedContent, Renderable, StaticImage};
use crate::com::{Displayable, GuiContent};
use crate::gui::glium_area::renderable::AllocatedTextures;
use crate::gui::{Gui, GUI};

#[derive(Copy, Clone)]
pub(super) struct Vertex {
    position: [f32; 2],
    tex_coords: [f32; 2],
}

implement_vertex!(Vertex, position, tex_coords);

pub(super) struct RenderContext {
    pub vertices: VertexBuffer<Vertex>,
    pub program: Program,
    pub indices: IndexBuffer<u8>,
    pub context: Rc<glium::backend::Context>,
}

impl Facade for &RenderContext {
    fn get_context(&self) -> &Rc<glium::backend::Context> {
        &self.context
    }
}

pub struct Renderer {
    backend: super::GliumArea,
    gui: Rc<Gui>,
    // TODO -- move this over to Gui.displayed,
    // this is just here to make rebasing easier and require less diffs
    pub(in super::super) displayed: DisplayedContent,
    // GTK does something really screwy, so if we need to invalidate once we'll need to do it again
    // next draw.
    invalidated: bool,
    pub(super) render_context: OnceCell<RenderContext>,
    // This is another reference to the same context in render_context.context
    // Really only necessary for initialization and to better compartmentalize this struct.
    context: Rc<glium::backend::Context>,
}

impl Facade for &Renderer {
    fn get_context(&self) -> &Rc<glium::backend::Context> {
        &self.context
    }
}

impl Renderer {
    fn new(context: Rc<glium::backend::Context>, backend: super::GliumArea) -> Self {
        let gui = GUI.with(|g| g.get().expect("Can't realize GliumArea without Gui").clone());

        let rend = Self {
            backend,
            gui,
            displayed: DisplayedContent::default(),
            invalidated: false,
            render_context: OnceCell::new(),
            context: context.clone(),
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

            fragment: "
                #version 140
                uniform sampler2D tex;
                in vec2 v_tex_coords;
                out vec4 f_color;
                void main() {
                    f_color = texture(tex, v_tex_coords);
                }
            "
        },)
        .unwrap();

        let indices =
            glium::IndexBuffer::new(&&rend, PrimitiveType::TriangleStrip, &[1, 2, 0, 3]).unwrap();

        assert!(
            rend.render_context
                .set(RenderContext { context, vertices, program, indices })
                .is_ok()
        );

        unsafe {
            let load_1 = Instant::now();
            (&rend).get_context().exec_in_context(|| {
                println!("load {:?}", load_1.elapsed());
                let load = Instant::now();
                gl::load_with(|symbol| rend.backend.get_proc_address(symbol) as *const _);
                println!("load {:?}", load.elapsed());
            });
        }

        rend
    }

    pub fn update_displayed(&mut self, content: &GuiContent) {
        use Displayable::*;
        use GuiContent::{Multiple, Single};

        let map_displayable = |d: &Displayable, at: AllocatedTextures| match d {
            Image(img) => Renderable::Image(StaticImage::new(img.clone(), at)),
            Animation(a) => Renderable::Animation(super::renderable::Animation::new(
                a,
                self.gui.animation_playing.get(),
            )),
            Video(_) | Error(_) | Nothing => Renderable::Nothing,
            Pending(res) => Renderable::Pending(*res),
        };

        let take_old_renderable = |d: &Displayable, old: &mut Vec<Renderable>| {
            for (i, o) in old.iter_mut().enumerate() {
                if o.matches(d) {
                    return Some(old.swap_remove(i));
                }
            }

            None
        };

        let mut db = &mut self.displayed;
        match (&mut db, &content) {
            (DisplayedContent::Single(old), Single(d)) => {
                if old.matches(d) {
                    // This should only really happen in the cases where things aren't loaded yet
                    error!("Old single content matches new displayable");
                }
                *db = DisplayedContent::Single(map_displayable(d, old.take_textures()))
            }
            (DisplayedContent::Multiple(old), Single(d)) => {
                let r = take_old_renderable(d, old)
                    .unwrap_or_else(|| map_displayable(d, AllocatedTextures::default()));
                *db = DisplayedContent::Single(r);
            }
            (DisplayedContent::Single(s), Multiple { visible, .. }) => {
                let visible = visible
                    .iter()
                    .map(|d| {
                        if s.matches(d) {
                            std::mem::take(s)
                        } else {
                            map_displayable(d, AllocatedTextures::default())
                        }
                    })
                    .collect();
                *db = DisplayedContent::Multiple(visible);
            }
            (DisplayedContent::Multiple(old), Multiple { visible, .. }) => {
                let visible = visible
                    .iter()
                    .map(|d| {
                        take_old_renderable(d, old)
                            .unwrap_or_else(|| map_displayable(d, AllocatedTextures::default()))
                    })
                    .collect();
                *db = DisplayedContent::Multiple(visible);
            }
        }
    }

    fn drop_textures(&mut self) {
        self.displayed.invalidate()
    }

    fn invalidate(&mut self) {
        self.drop_textures();
        self.invalidated = true;
    }

    fn draw(&mut self) {
        let context = self.context.clone();
        let (w, h) = context.get_framebuffer_dimensions();

        let mut frame = Frame::new(context, (w, h));
        let mut drew_something = false;

        let bg = self.gui.bg.get();
        frame.clear_color(
            bg.red() * bg.alpha(),
            bg.green() * bg.alpha(),
            bg.blue() * bg.alpha(),
            bg.alpha(),
        );

        {
            let layout_manager = self.gui.scroll_state.borrow();
            let mut layouts = layout_manager.layout_iter();


            let mut render = |d: &mut Renderable| {
                let layout = layouts.next().expect("Layout not defined for all displayed pages.");
                match d {
                    Renderable::Image(tc) => {
                        drew_something = true;

                        tc.draw(
                            self.render_context.get().unwrap(),
                            &mut frame,
                            layout,
                            (w, h).into(),
                        );
                    }
                    Renderable::Animation(ac) => {
                        drew_something = true;

                        Animation::draw(
                            ac,
                            self.render_context.get().unwrap(),
                            &mut frame,
                            layout,
                            (w, h).into(),
                        );
                    }
                    Renderable::Pending(_) | Renderable::Nothing => {}
                }
            };

            match &mut self.displayed {
                DisplayedContent::Single(r) => render(r),
                DisplayedContent::Multiple(visible) => visible.iter_mut().for_each(render),
            }
        }

        frame.finish().unwrap();
        if drew_something {
            let old_now = self.gui.last_action.take();
            if let Some(old_now) = old_now {
                let dur = old_now.elapsed();

                if dur > Duration::from_secs(10) {
                    // Probably wasn't an action that changed anything. Don't log anything.
                } else if dur > Duration::from_millis(100) {
                    info!("Took {:?} from action to drawable change.", dur);
                } else if dur > Duration::from_millis(20) {
                    debug!("Took {:?} from action to drawable change.", dur);
                } else {
                    trace!("Took {:?} from action to drawable change.", dur);
                }
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
        }
    }
}

#[derive(Default)]
pub struct GliumGLArea {
    pub(in crate::gui) renderer: RefCell<Option<Renderer>>,
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
}
