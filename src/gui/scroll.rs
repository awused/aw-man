use std::cmp::{max, min};
use std::mem::ManuallyDrop;
use std::rc::{Rc, Weak};
use std::time::{Duration, Instant};

use gtk::glib::Continue;
use gtk::prelude::{WidgetExt, WidgetExtManual};
use gtk::TickCallbackId;
use once_cell::sync::Lazy;

use super::Gui;
use crate::com::{CommandResponder, DebugIgnore, Direction, Fit, ManagerAction, Res, TargetRes};
use crate::config::CONFIG;

static SCROLL_AMOUNT: Lazy<i32> = Lazy::new(|| CONFIG.scroll_amount.get() as i32);

// TODO -- consider making scrolling take longer if multiple events stack.
static SCROLL_DURATION: Duration = Duration::from_millis(150);

#[derive(Debug)]
enum Motion {
    Stationary,
    Smooth {
        // Pairs for (start, end)
        // Final values may be unreachable.
        x: (i32, i32),
        y: (i32, i32),
        // May be more than SCROLL_DURATION in the past.
        start: Instant,
        // Whenever the last step was taken.
        step: Instant,
        tick_id: ManuallyDrop<TickCallbackId>,
    },
    Dragging {
        // Drag gestures are given as a series of offsets relative to the start of the drag.
        // Store the previous one (truncated towards zero) to convert them into a series of diffs.
        offset: (i32, i32),
    },
    // Pad scrolling if we need to track remainders here.
    // In case we need it specifically for touchpad scrolling? Make this a separate enum?
    // remainder: (i32, i32)
}

// impl Drop, cancel smooth scroll callback
impl Drop for Motion {
    fn drop(&mut self) {
        if let Self::Smooth { tick_id, .. } = self {
            // We're dropping it, so this is safe
            unsafe {
                ManuallyDrop::take(tick_id).remove();
            }
        }
    }
}

#[derive(Debug)]
pub(super) enum ScrollContents {
    Single(Res),
    _Continuous {
        // For now, continuous scrolling only allows for it in the vertical direction.
        prev: bool,
        visible: Vec<Res>,
        next: bool,
    },
}

impl ScrollContents {
    // Find the total res from fitting all visible images using the current fit strategy.
    //
    // Continuous scrolling might lead to unusual behaviour if images have different widths and
    // some of them are wider than the monitor.
    fn total_res(&self, target_res: TargetRes) -> Res {
        match self {
            ScrollContents::Single(r) => r.fit_inside(target_res),
            ScrollContents::_Continuous { visible, .. } => {
                let mut max_x = 0;
                let mut sum_y = 0;

                for v in visible {
                    let t = v.fit_inside(target_res);

                    max_x = max(max_x, t.w);
                    sum_y += t.h;
                }

                (max_x, sum_y).into()
            }
        }
    }
}

enum Edges {
    Top,
    Bottom,
    Neither,
}

// This struct only tracks what is immediately necessary for scrolling.
#[derive(Debug)]
pub(super) struct ScrollState {
    // The current visible offsets of the upper left corner of the viewport relative to the upper
    // left corner of the displayed content plus letterboxing.
    // These are necessarily non-negative.
    pub(super) x: u32,
    pub(super) y: u32,

    motion: Motion,

    // The maximum and minimum scroll bounds for the currently visible images.
    bounds: Res,
    contents: ScrollContents,
    target_res: TargetRes,

    // TODO -- could unbox this more easily with existential types, but not worth full generics.
    add_tick_callback: DebugIgnore<Box<dyn Fn() -> TickCallbackId>>,
}

#[derive(Debug, Eq, PartialEq, Copy, Clone)]
pub(super) enum ScrollPagination {
    Forwards,
    Backwards,
}

#[derive(Debug, Eq, PartialEq, Copy, Clone)]
pub(super) enum ScrollPos {
    Start,
    End,
    Maintain,
    _Continuous(ScrollPagination),
}

#[derive(Debug, Eq, PartialEq, Copy, Clone)]
enum ScrollResult {
    Applied,
    MustPaginate(ScrollPagination),
}

impl ScrollState {
    pub(super) fn new(gui: Weak<Gui>) -> Self {
        let add_tick_callback: DebugIgnore<Box<dyn Fn() -> TickCallbackId>> =
            DebugIgnore(Box::new(move || gui.upgrade().unwrap().add_tick_callback()));

        Self {
            x: 0,
            y: 0,
            motion: Motion::Stationary,
            bounds: (0, 0).into(),
            target_res: (0, 0, Fit::Container).into(),
            contents: ScrollContents::Single((0, 0).into()),
            add_tick_callback,
        }
    }

    fn update_contents(&mut self, contents: ScrollContents, pos: ScrollPos) {
        match self.contents {
            ScrollContents::_Continuous { .. } => (),
            ScrollContents::Single(_) => self.motion = Motion::Stationary,
        };

        let _old_contents = std::mem::replace(&mut self.contents, contents);
        let old_bounds = self.bounds;

        let fitted_res = self.contents.total_res(self.target_res);

        self.bounds = (
            fitted_res.w.saturating_sub(self.target_res.res.w),
            fitted_res.h.saturating_sub(self.target_res.res.h),
        )
            .into();


        match pos {
            ScrollPos::Start => {
                self.x = 0;
                self.y = 0;
            }
            ScrollPos::End => {
                // TODO -- needs to be bounds of the first content element, not them all.
                self.x = self.bounds.w;
                self.y = self.bounds.h;
            }
            ScrollPos::Maintain => {
                self.readjust_scroll_in_place(old_bounds);
            }
            ScrollPos::_Continuous(ScrollPagination::Forwards) => {
                todo!()
            }
            ScrollPos::_Continuous(ScrollPagination::Backwards) => {
                todo!()
            }
        }
    }

    fn zero(&mut self) {
        self.update_contents(ScrollContents::Single((0, 0).into()), ScrollPos::Start)
    }

    fn update_container(&mut self, target_res: TargetRes) {
        // We resized, stop any scrolling now.
        self.motion = Motion::Stationary;
        self.target_res = target_res;

        let fitted_res = self.contents.total_res(self.target_res);
        let old_bounds = self.bounds;

        self.bounds = (
            fitted_res.w.saturating_sub(self.target_res.res.w),
            fitted_res.h.saturating_sub(self.target_res.res.h),
        )
            .into();

        self.readjust_scroll_in_place(old_bounds);
    }

    pub fn readjust_scroll_in_place(&mut self, old_bounds: Res) {
        // TODO -- single -> single, leave as is.
        // single->continuous, adjust based on the first image only.
        // continuous -> single, clamp to first image.
        // continuous -> continuous, figure out what the current center image is and
        // attempt to adjust.
        // Use a completely separate function for stepping one way or another.
        // TODO -- really should be somehow based on the center element, but first element
        // is probably good enough?
        let xpercent = if old_bounds.w > 0 { self.x as f64 / old_bounds.w as f64 } else { 0.0 };
        let ypercent = if old_bounds.h > 0 { self.y as f64 / old_bounds.h as f64 } else { 0.0 };

        self.x = (xpercent * self.bounds.w as f64).round() as u32;
        self.y = (ypercent * self.bounds.h as f64).round() as u32;
    }

    // These return false when no scrolling was done because we were already at the edge.
    // SHOULD: return false only if an explicit page up/down needs to be done right now.
    // (not continuous OR bounded [until continuous supports all content]) AND ((target already < 0
    // or > bounds) OR (already on edge))
    //
    // In continuous mode the smooth scrolling callback will be responsible for switching pages.
    fn apply_scroll(&mut self, dx: i32, dy: i32) {
        if let Motion::Smooth {
            x: (sx, tx), y: (sy, ty), start, step, ..
        } = &mut self.motion
        {
            *sx = self.x as i32;
            *sy = self.y as i32;
            *tx += dx;
            *ty += dy;
            *start = *step;
        } else {
            let scroll_start = Instant::now();
            let tick_id = ManuallyDrop::new((self.add_tick_callback)());
            self.motion = Motion::Smooth {
                x: (self.x as i32, self.x as i32 + dx),
                y: (self.y as i32, self.y as i32 + dy),
                start: scroll_start,
                step: scroll_start,
                tick_id,
            };
        }
    }

    fn scroll_down(&mut self) -> ScrollResult {
        let ty = if let Motion::Smooth { y: (_, ty), .. } = self.motion {
            ty
        } else {
            self.y as i32
        };

        // Add SCROLL_AMOUNT to make it harder to accidentally switch pages, but still possible.
        if self.y == self.bounds.h || ty > self.bounds.h as i32 + *SCROLL_AMOUNT {
            return ScrollResult::MustPaginate(ScrollPagination::Forwards);
        }

        self.apply_scroll(0, *SCROLL_AMOUNT);

        ScrollResult::Applied
    }

    fn scroll_up(&mut self) -> ScrollResult {
        let ty = if let Motion::Smooth { y: (_, ty), .. } = self.motion {
            ty
        } else {
            self.y as i32
        };

        // Add SCROLL_AMOUNT to make it harder to accidentally switch pages, but still possible.
        if self.y == 0 || ty < -*SCROLL_AMOUNT {
            return ScrollResult::MustPaginate(ScrollPagination::Backwards);
        }

        self.apply_scroll(0, -*SCROLL_AMOUNT);

        ScrollResult::Applied
    }

    fn scroll_right(&mut self) {
        let tx = if let Motion::Smooth { x: (_, tx), .. } = self.motion {
            tx
        } else {
            self.x as i32
        };

        if self.x == self.bounds.w || tx > self.bounds.w as i32 {
            return;
        }

        self.apply_scroll(*SCROLL_AMOUNT, 0);
    }

    fn scroll_left(&mut self) {
        let tx = if let Motion::Smooth { x: (_, tx), .. } = self.motion {
            tx
        } else {
            self.x as i32
        };

        if self.x == 0 || tx < 0 {
            return;
        }

        self.apply_scroll(-*SCROLL_AMOUNT, 0);
    }

    fn pad_scroll(&mut self, x: f64, y: f64) {
        let dx = (x * *SCROLL_AMOUNT as f64).round() as i32;
        let dy = (y * *SCROLL_AMOUNT as f64).round() as i32;

        self.motion = Motion::Stationary;
        // TODO -- handle dragging against the edge of an image. Clamp drag_offset to the value
        // actually applied then return the remainder or a bounding indicator.
        self.apply_delta(dx, dy);
    }

    pub(super) fn start_drag(&mut self) {
        self.motion = Motion::Dragging { offset: (0, 0) };
    }

    fn apply_drag_update(&mut self, ofx: f64, ofy: f64) {
        let drag_offset = if let Motion::Dragging { offset } = &self.motion {
            offset
        } else {
            // This may happen if the user has multiple scroll devices. Not worth handling.
            debug!("Got dragging event outside of dragging scroll mode.");
            return;
        };

        // Round towards zero
        let ofx = ofx.trunc() as i32;
        let ofy = ofy.trunc() as i32;

        let dx = ofx - drag_offset.0;
        let dy = ofy - drag_offset.1;

        let (rx, ry) = self.apply_delta(dx, dy);

        self.motion = Motion::Dragging { offset: (ofx - rx, ofy - ry) };
    }

    // Returns the remainder after attempting to apply the delta.
    fn apply_delta(&mut self, dx: i32, dy: i32) -> (i32, i32) {
        let tx = self.x as i32 + dx;
        let ty = self.y as i32 + dy;

        self.x = min(max(tx, 0) as u32, self.bounds.w);
        self.y = min(max(ty, 0) as u32, self.bounds.h);

        (tx - self.x as i32, ty - self.y as i32)
    }

    fn smooth_step(&mut self) {
        let now = Instant::now();

        let (x, y, start) = if let Motion::Smooth { x, y, start, ref mut step, .. } = self.motion {
            *step = now;
            (x, y, start)
        } else {
            unreachable!();
        };

        let scale =
            f32::min((now - start).as_millis() as f32 / SCROLL_DURATION.as_millis() as f32, 1.0);

        self.x =
            min(max(((x.1 - x.0) as f32 * scale).round() as i32 + x.0, 0) as u32, self.bounds.w);
        self.y =
            min(max(((y.1 - y.0) as f32 * scale).round() as i32 + y.0, 0) as u32, self.bounds.h);

        trace!("Smooth scroll step: {scale} {x:?} {y:?} {} {}", self.x, self.y);

        if now > start + SCROLL_DURATION {
            // We're done if we're in Single mode or if we're in Continuous mode and there's
            // nowhere to scroll.
            match self.contents {
                ScrollContents::Single(_) => self.motion = Motion::Stationary,
                ScrollContents::_Continuous { .. } => todo!(),
            }
        }
    }

    fn touched_edges(&self) -> Edges {
        if self.bounds.h == 0 {
            return Edges::Neither;
        }

        let ty = if let Motion::Smooth { y: (_, ty), .. } = self.motion {
            ty
        } else {
            self.y as i32
        };


        match self.contents {
            ScrollContents::Single(_) => {
                if ty <= 0 {
                    Edges::Top
                } else if ty >= self.bounds.h as i32 {
                    Edges::Bottom
                } else {
                    Edges::Neither
                }
            }
            ScrollContents::_Continuous { .. } => todo!(),
        }
        // Top/bottom only in continuous mode when bounded.
    }
}

impl Gui {
    pub(super) fn update_scroll_container(self: &Rc<Self>, target_res: TargetRes) {
        let mut sb = self.scroll_state.borrow_mut();
        sb.update_container(target_res);
        self.update_edge_indicator(&sb);
    }

    pub(super) fn zero_scroll(self: &Rc<Self>) {
        let mut sb = self.scroll_state.borrow_mut();
        sb.zero();
        self.update_edge_indicator(&sb);
    }

    pub(super) fn update_scroll_contents(
        self: &Rc<Self>,
        contents: ScrollContents,
        pos: ScrollPos,
    ) {
        let mut sb = self.scroll_state.borrow_mut();
        sb.update_contents(contents, pos);
        self.update_edge_indicator(&sb);
    }

    pub(super) fn scroll_down(self: &Rc<Self>, fin: Option<CommandResponder>) {
        let mut sb = self.scroll_state.borrow_mut();
        match sb.scroll_down() {
            ScrollResult::Applied => {
                self.update_edge_indicator(&sb);
            }
            ScrollResult::MustPaginate(_) => {
                self.scroll_motion_target.set(ScrollPos::Start);
                self.manager_sender
                    .send((ManagerAction::MovePages(Direction::Forwards, 1), fin))
                    .expect("Failed to send from Gui to Manager")
            }
        }
    }

    pub(super) fn scroll_up(self: &Rc<Self>, fin: Option<CommandResponder>) {
        let mut sb = self.scroll_state.borrow_mut();
        match sb.scroll_up() {
            ScrollResult::Applied => {
                self.update_edge_indicator(&sb);
            }
            ScrollResult::MustPaginate(_) => {
                self.scroll_motion_target.set(ScrollPos::End);
                self.manager_sender
                    .send((ManagerAction::MovePages(Direction::Backwards, 1), fin))
                    .expect("Failed to send from Gui to Manager")
            }
        }
    }

    pub(super) fn scroll_right(self: &Rc<Self>, _fin: Option<CommandResponder>) {
        self.scroll_state.borrow_mut().scroll_right()
    }

    pub(super) fn scroll_left(self: &Rc<Self>, _fin: Option<CommandResponder>) {
        self.scroll_state.borrow_mut().scroll_left()
    }

    pub(super) fn discrete_scroll(self: &Rc<Self>, x: f64, y: f64) {
        trace!("Started responding to scroll");
        self.last_action.set(Some(Instant::now()));

        if y > 0.0 {
            self.scroll_down(None);
        } else if y < 0.0 {
            self.scroll_up(None);
        } else if x > 0.0 {
            self.scroll_right(None);
        } else if x < 0.0 {
            self.scroll_left(None);
        }
    }

    pub(super) fn pad_scroll(self: &Rc<Self>, x: f64, y: f64) {
        // TODO -- allow for continuous scroll mode here
        let mut sb = self.scroll_state.borrow_mut();
        sb.pad_scroll(x, y);
        self.update_edge_indicator(&sb);
        self.canvas.queue_draw();
    }

    pub(super) fn drag_update(self: &Rc<Self>, x: f64, y: f64) {
        // TODO -- allow for continuous scroll mode here
        let mut sb = self.scroll_state.borrow_mut();
        sb.apply_drag_update(x, y);
        self.update_edge_indicator(&sb);
        self.canvas.queue_draw();
    }

    fn add_tick_callback(self: Rc<Self>) -> TickCallbackId {
        trace!("Beginning smooth scrolling");
        let g = self.clone();
        self.canvas.add_tick_callback(move |_canvas, _clock| g.tick_callback())
    }

    fn update_edge_indicator(self: &Rc<Self>, scroll: &ScrollState) {
        match scroll.touched_edges() {
            Edges::Top => self.edge_indicator.set_text("🡅"),
            Edges::Bottom => self.edge_indicator.set_text("🡇"),
            Edges::Neither => self.edge_indicator.set_text(""),
        }
    }

    fn tick_callback(self: &Rc<Self>) -> Continue {
        self.scroll_state.borrow_mut().smooth_step();
        self.canvas.queue_draw();
        Continue(true)
    }
}
