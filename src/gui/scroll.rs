use std::cmp::{max, min};
use std::mem::ManuallyDrop;
use std::rc::{Rc, Weak};
use std::time::{Duration, Instant};

use gtk::glib::Continue;
use gtk::prelude::{WidgetExt, WidgetExtManual};
use gtk::TickCallbackId;
use once_cell::sync::Lazy;

use super::Gui;
use crate::com::{
    CommandResponder, DebugIgnore, Direction, DisplayMode, Fit, GuiContent, ManagerAction,
    OffscreenContent, Pagination, Res, ScrollMotionTarget, TargetRes,
};
use crate::config::CONFIG;

static SCROLL_AMOUNT: Lazy<i32> = Lazy::new(|| CONFIG.scroll_amount.get() as i32);

static SCROLL_DURATION: Duration = Duration::from_millis(166);

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
    Multiple {
        // prev and next are true if we can continue to scroll into the next page.
        prev: Option<Res>,
        visible: Vec<Res>,
        next: Option<Res>,
    },
}

impl ScrollContents {
    // Find the total res from fitting all visible images using the current fit strategy and the
    // current scroll bounds.
    //
    // Continuous vertical or horizontal scrolling might lead to unusual behaviour if some images
    // are of different resolutions.
    fn fit(&self, target_res: TargetRes, mode: DisplayMode) -> (Res, Res, Rect) {
        match self {
            ScrollContents::Single(r) => {
                let fitted = r.fit_inside(target_res);
                let bounds: Res = (
                    fitted.w.saturating_sub(target_res.res.w),
                    fitted.h.saturating_sub(target_res.res.h),
                )
                    .into();
                let true_bounds = Rect {
                    top: 0,
                    left: 0,
                    bottom: bounds.h as i32,
                    right: bounds.w as i32,
                };
                (fitted, bounds, true_bounds)
            }
            ScrollContents::Multiple { visible, next, prev } => match mode {
                DisplayMode::Single => unreachable!(),
                DisplayMode::VerticalStrip => {
                    let first = visible[0].fit_inside(target_res);
                    let mut max_x = first.w;
                    let mut sum_y = first.h;

                    for v in &visible[1..] {
                        let t = v.fit_inside(target_res);

                        max_x = max(max_x, t.w);
                        sum_y += t.h;
                    }

                    let fitted: Res = (max_x, sum_y).into();

                    // Subtract 1 from first.h to avoid scrolling one pixel past the bottom
                    let pagination_bounds: Res = (
                        fitted.w.saturating_sub(target_res.res.w),
                        min(fitted.h.saturating_sub(target_res.res.h), first.h.saturating_sub(1)),
                    )
                        .into();

                    // We don't consider off-screen elements for width. Which could be weird,
                    // but it'll be weird no matter what we do.

                    if let Some(r) = next {
                        sum_y += r.fit_inside(target_res).h;
                    }

                    let top = prev.map_or(0, |r| -(r.fit_inside(target_res).h as i32));

                    let true_bounds = Rect {
                        top,
                        left: 0,
                        bottom: sum_y.saturating_sub(target_res.res.h) as i32,
                        right: pagination_bounds.w as i32,
                    };


                    (fitted, pagination_bounds, true_bounds)
                }
            },
        }
    }

    // Used when moving back using Jump/PreviousPage/etc.
    fn first_element_end_position(&self, target_res: TargetRes, mode: DisplayMode) -> (u32, u32) {
        let fitted = self.first_res(target_res, mode);

        (
            fitted.w.saturating_sub(target_res.res.w),
            fitted.h.saturating_sub(target_res.res.h),
        )
    }

    fn first_res(&self, target_res: TargetRes, mode: DisplayMode) -> Res {
        match self {
            ScrollContents::Single(r) => r.fit_inside(target_res),
            ScrollContents::Multiple { visible, .. } => match mode {
                DisplayMode::Single => unreachable!(),
                DisplayMode::VerticalStrip => visible[0].fit_inside(target_res),
            },
        }
    }
}

enum Edges {
    Top,
    Bottom,
    // TODO -- None/Left/Right
    Neither,
}

#[derive(Debug, Default)]
struct Rect {
    top: i32,
    bottom: i32,
    left: i32,
    right: i32,
}

// TODO -- rename to LayoutManager or Layout
// Eventually include videos and animations
// This struct only tracks what is immediately necessary for scrolling and laying out currently
// visible pages.
#[derive(Debug)]
pub(super) struct ScrollState {
    // The current visible offsets of the upper left corner of the viewport relative to the upper
    // left corner of the displayed content plus letterboxing.
    // These are necessarily non-negative.
    x: i32,
    y: i32,
    // Used to store the current scroll position when swapping between views where the image is
    // fully visible and bounds become 0.
    saved_positions: (f64, f64),

    motion: Motion,

    // The maximum and minimum scroll bounds for the currently known images. Scrolling can't fall
    // outside this range even at the cost of visible hitching.
    true_bounds: Rect,
    // The maximum and minimum scroll bounds before paginating to another page. Scrolling in some
    // modes can fall outside of these bounds, but this will result in continuous mode paginations.
    page_bounds: Res,

    contents: ScrollContents,
    mode: DisplayMode,
    fitted_res: Res,
    target_res: TargetRes,

    // TODO -- could unbox this more easily with existential types, but not worth full generics.
    add_tick_callback: DebugIgnore<Box<dyn Fn() -> TickCallbackId>>,
}


#[derive(Debug, Eq, PartialEq, Copy, Clone)]
enum ScrollResult {
    NoOp,
    Applied,
    NormalPagination(Pagination),
}

impl ScrollResult {
    const fn should_apply(self) -> bool {
        match self {
            Self::NoOp | Self::NormalPagination(_) => false,
            Self::Applied => true,
        }
    }
}

impl ScrollState {
    pub(super) fn new(gui: Weak<Gui>) -> Self {
        let add_tick_callback: DebugIgnore<Box<dyn Fn() -> TickCallbackId>> =
            DebugIgnore(Box::new(move || gui.upgrade().unwrap().add_tick_callback()));

        Self {
            x: 0,
            y: 0,
            saved_positions: (0.0, 0.0),

            motion: Motion::Stationary,

            mode: DisplayMode::Single,
            target_res: (0, 0, Fit::Container).into(),
            contents: ScrollContents::Single((0, 0).into()),

            fitted_res: (0, 0).into(),
            page_bounds: (0, 0).into(),
            true_bounds: Rect::default(),

            add_tick_callback,
        }
    }

    fn update_contents(
        &mut self,
        contents: ScrollContents,
        pos: ScrollMotionTarget,
        mode: DisplayMode,
    ) {
        // Cancel any ongoing scrolling except when continuously scrolling and paginating.
        match (&self.contents, pos) {
            (ScrollContents::Multiple { .. }, ScrollMotionTarget::Continuous(_)) => (),
            _ => self.motion = Motion::Stationary,
        }

        let old_first_res = self.contents.first_res(self.target_res, self.mode);
        let old_page_bounds = self.page_bounds;

        self.mode = mode;
        self.contents = contents;
        (self.fitted_res, self.page_bounds, self.true_bounds) =
            self.contents.fit(self.target_res, mode);

        match pos {
            ScrollMotionTarget::Start => {
                self.x = 0;
                self.y = 0;
                self.saved_positions = (0.0, 0.0);
            }
            ScrollMotionTarget::End => {
                let end = self.contents.first_element_end_position(self.target_res, self.mode);
                self.x = end.0 as i32;
                self.y = end.1 as i32;

                // This seems weird, but it's probably closer to my intention most of the time.
                // If the current page isn't scrolling when the user reaches it this will treat it
                // the same as if I paged down.
                // If it is scrolling, this will not matter and it will stay wherever it is placed.
                self.saved_positions = (0.0, 0.0);
            }
            ScrollMotionTarget::Maintain => {
                self.readjust_scroll_in_place(old_first_res, old_page_bounds)
            }
            ScrollMotionTarget::Continuous(p) => {
                let (dx, dy) = match (p, self.mode.vertical_pagination()) {
                    (Pagination::Forwards, true) => (0, -(old_page_bounds.h as i32 + 1)),
                    (Pagination::Forwards, false) => (-(old_page_bounds.w as i32 + 1), 0),
                    (Pagination::Backwards, true) => (0, self.page_bounds.h as i32 + 1),
                    (Pagination::Backwards, false) => (self.page_bounds.w as i32 + 1, 0),
                };

                // This clamping could change the scroll position if the user reversed directions
                // before the new page was ready, but that's probably not worth fixing. Best to
                // avoid the case where (x,y) + (dx,dy) lands in an area that should
                // be paginating.
                self.x = (self.x + dx as i32).clamp(0, self.page_bounds.w as i32);
                // self.x = (self.x + dx as i32).clamp(self.true_bounds.left,
                // self.true_bounds.right);
                self.y = (self.y + dy as i32).clamp(0, self.page_bounds.h as i32);
                // self.y = (self.y + dy as i32).clamp(self.true_bounds.top,
                // self.true_bounds.bottom);

                // This shouldn't actually matter unless we're going from a state the user couldn't
                // normally scroll to to this
                if self.mode.vertical_pagination() {
                    self.saved_positions.1 = 0.0;
                } else {
                    self.saved_positions.0 = 0.0;
                }

                match &mut self.motion {
                    Motion::Stationary | Motion::Dragging { .. } => {}
                    Motion::Smooth { x, y, .. } => {
                        x.0 += dx;
                        x.1 += dx;
                        y.0 += dy;
                        y.1 += dy;
                    }
                }
            }
        }
    }

    fn zero(&mut self) {
        self.update_contents(
            ScrollContents::Single((0, 0).into()),
            ScrollMotionTarget::Start,
            self.mode,
        )
    }

    fn update_container(&mut self, target_res: TargetRes) {
        // We resized, stop any scrolling now.
        self.motion = Motion::Stationary;

        let old_bounds = self.page_bounds;
        let old_first_res = self.contents.first_res(self.target_res, self.mode);

        self.target_res = target_res;
        (self.fitted_res, self.page_bounds, self.true_bounds) =
            self.contents.fit(self.target_res, self.mode);

        self.readjust_scroll_in_place(old_first_res, old_bounds);
    }

    pub fn readjust_scroll_in_place(&mut self, old_res: Res, old_bounds: Res) {
        let new_res = self.contents.first_res(self.target_res, self.mode);

        if old_bounds.w > 0 {
            self.saved_positions.0 = min(self.x, old_res.w as i32) as f64 / old_res.w as f64;
        }
        if old_bounds.h > 0 {
            self.saved_positions.1 = min(self.y, old_res.h as i32) as f64 / old_res.h as f64;
        }

        self.x = (self.saved_positions.0 * new_res.w as f64).round() as i32;
        self.y = ((self.saved_positions.1 * new_res.h as f64).round() as i32)
            .clamp(0, self.page_bounds.h as i32);
    }

    // In continuous mode the smooth scrolling callback will be responsible for switching pages.
    fn apply_smooth_scroll(&mut self, dx: i32, dy: i32) -> ScrollResult {
        let (tx, ty) = if let Motion::Smooth { x: (_, tx), y: (_, ty), .. } = self.motion {
            (tx + dx, ty + dy)
        } else {
            (self.x + dx, self.y + dy)
        };

        let r = self.check_pagination(dx, dy, tx, ty);
        if !r.should_apply() {
            return r;
        }

        if let Motion::Smooth { x, y, start, step, .. } = &mut self.motion {
            *x = (self.x, tx);
            *y = (self.y, ty);
            *start = *step;
        } else {
            let scroll_start = Instant::now();
            let tick_id = ManuallyDrop::new((self.add_tick_callback)());

            self.motion = Motion::Smooth {
                x: (self.x, tx),
                y: (self.y, ty),
                start: scroll_start,
                step: scroll_start,
                tick_id,
            };
        }

        r
    }

    fn check_pagination(&self, dx: i32, dy: i32, tx: i32, ty: i32) -> ScrollResult {
        if self.mode.vertical_pagination() {
            match dy {
                0 => {
                    // tx - dx is not necessarily equal to self.x
                    if (tx - dx <= 0 && dx <= 0)
                        || (tx - dx >= self.page_bounds.w as i32 && dx >= 0)
                    {
                        ScrollResult::NoOp
                    } else {
                        ScrollResult::Applied
                    }
                }
                1.. => {
                    // Positive = Down = Forwards
                    if self.true_bounds.bottom == self.page_bounds.h as i32
                        && (self.y == self.true_bounds.bottom
                            || ty > self.true_bounds.bottom + *SCROLL_AMOUNT)
                    {
                        ScrollResult::NormalPagination(Pagination::Forwards)
                    } else {
                        ScrollResult::Applied
                    }
                }
                _ => {
                    if self.true_bounds.top == 0 && (self.y == 0 || ty < -*SCROLL_AMOUNT) {
                        ScrollResult::NormalPagination(Pagination::Backwards)
                    } else {
                        ScrollResult::Applied
                    }
                }
            }
        } else {
            unimplemented!()
        }
    }

    fn pad_scroll(&mut self, x: f64, y: f64) -> Option<Pagination> {
        let dx = (x * *SCROLL_AMOUNT as f64).round() as i32;
        let dy = (y * *SCROLL_AMOUNT as f64).round() as i32;

        self.motion = Motion::Stationary;
        // TODO -- handle dragging against the edge of an image. Clamp drag_offset to the value
        // actually applied then return the remainder or a bounding indicator.
        self.apply_delta(dx, dy).2
    }

    pub(super) fn start_drag(&mut self) {
        self.motion = Motion::Dragging { offset: (0, 0) };
    }

    fn apply_drag_update(&mut self, ofx: f64, ofy: f64) -> Option<Pagination> {
        let drag_offset = if let Motion::Dragging { offset } = &self.motion {
            offset
        } else {
            // This may happen if the user has multiple scroll devices. Not worth handling.
            debug!("Got dragging event outside of dragging scroll mode.");
            return None;
        };

        // Round towards zero
        let ofx = ofx.trunc() as i32;
        let ofy = ofy.trunc() as i32;

        let dx = ofx - drag_offset.0;
        let dy = ofy - drag_offset.1;

        let (rx, ry, p) = self.apply_delta(dx, dy);

        self.motion = Motion::Dragging { offset: (ofx - rx, ofy - ry) };
        p
    }

    // Returns the remainder after attempting to apply the delta.
    fn apply_delta(&mut self, dx: i32, dy: i32) -> (i32, i32, Option<Pagination>) {
        let tx = self.x + dx;
        let ty = self.y + dy;

        let (old_x, old_y) = (self.x, self.y);

        self.x = tx.clamp(self.true_bounds.left, self.true_bounds.right);
        self.y = ty.clamp(self.true_bounds.top, self.true_bounds.bottom);

        // TODO --check if we're not already paginating right now, this could result in weird
        // double scrolling if the user does something really weird in the gap between pagination
        // starting and finishing. Unlikely to be a problem in practice.
        let p = if (self.x < 0 && old_x >= 0) || (self.y < 0 && old_y >= 0) {
            Some(Pagination::Backwards)
        } else if (self.x > self.page_bounds.w as i32 && old_x <= self.page_bounds.w as i32)
            || (self.y > self.page_bounds.h as i32 && old_y <= self.page_bounds.h as i32)
        {
            Some(Pagination::Forwards)
        } else {
            None
        };

        (tx - self.x, ty - self.y, p)
    }

    // Returns a direction for continuous scrolling, if necessary
    fn smooth_step(&mut self) -> Option<Pagination> {
        // TODO -- pause scroll updates when a pagination request is pending
        let now = Instant::now();

        let (x, y, start) = if let Motion::Smooth { x, y, start, ref mut step, .. } = self.motion {
            *step = now;
            (x, y, start)
        } else {
            unreachable!();
        };

        let scale = f32::min(
            ((now - start).as_millis() as f32) as f32 / SCROLL_DURATION.as_millis() as f32,
            1.0,
        );

        let dx = ((x.1 - x.0) as f32 * scale).round() as i32;
        let dy = ((y.1 - y.0) as f32 * scale).round() as i32;

        let dx = x.0 + dx - self.x;
        let dy = y.0 + dy - self.y;

        let (_, _, p) = self.apply_delta(dx, dy);

        trace!(
            "Smooth scroll step: {scale} {:?} {x:?} {y:?} {} {}",
            now - start,
            self.x,
            self.y,
        );

        if scale == 1.0 {
            self.motion = Motion::Stationary;
        }

        p
    }

    fn touched_edges(&self) -> Edges {
        // TODO -- other modes.
        assert!(self.mode.vertical_pagination());
        if self.page_bounds.h == 0 {
            return Edges::Neither;
        }

        let ty = if let Motion::Smooth { y: (_, ty), .. } = self.motion { ty } else { self.y };

        match self.contents {
            ScrollContents::Single(_) => {
                if ty <= self.true_bounds.top {
                    Edges::Top
                } else if ty >= self.true_bounds.bottom as i32 {
                    Edges::Bottom
                } else {
                    Edges::Neither
                }
            }
            ScrollContents::Multiple { prev, next, .. } => {
                if ty <= 0 && prev.is_none() {
                    Edges::Top
                } else if ty >= self.page_bounds.h as i32 && next.is_none() {
                    Edges::Bottom
                } else {
                    Edges::Neither
                }
            }
        }
    }

    pub(super) const fn layout_iter(&self) -> LayoutIterator {
        // Only supports vertical continuous scrolling
        let v_off = if let ScrollContents::Multiple { prev: Some(_), .. } = self.contents {
            self.y.saturating_neg()
        } else {
            self.target_res.res.h.saturating_sub(self.fitted_res.h) as i32 / 2 - self.y
        };

        let upper_left = (
            self.target_res.res.w.saturating_sub(self.fitted_res.w) as i32 / 2 - self.x,
            v_off,
        );

        LayoutIterator {
            index: 0,
            state: self,
            upper_left,
            current_offset: (0, 0),
        }
    }
}

pub(super) struct LayoutIterator<'a> {
    index: usize,
    state: &'a ScrollState,
    upper_left: (i32, i32),
    current_offset: (i32, i32),
}

impl<'a> Iterator for LayoutIterator<'a> {
    type Item = (i32, i32, Res);

    fn next(&mut self) -> Option<Self::Item> {
        let layout = match &self.state.contents {
            ScrollContents::Single(r) => {
                let res = r.fit_inside(self.state.target_res);
                if self.index == 0 {
                    Some((self.upper_left.0, self.upper_left.1, res))
                } else {
                    None
                }
            }
            ScrollContents::Multiple { visible, .. } => {
                if let Some(v) = visible.get(self.index) {
                    let res = v.fit_inside(self.state.target_res);
                    let (mut ofx, ofy) = (
                        self.upper_left.0 + self.current_offset.0,
                        self.upper_left.1 + self.current_offset.1,
                    );

                    match self.state.mode {
                        DisplayMode::VerticalStrip => {
                            // If this is thinner than the other elements, center it.
                            // Might cause weird jumping around if some elements are wider than the
                            // screen, not much to be done there.
                            // res.w <= self.state.fitted_res.w
                            ofx += (self.state.fitted_res.w - res.w) as i32 / 2;

                            // For now only vertical continuous scrolling is allowed.
                            // This could be modified for horizontal or dual-page scrolling.
                            self.current_offset.1 += res.h as i32;
                        }
                        DisplayMode::Single => unreachable!(),
                    }

                    Some((ofx, ofy, res))
                } else {
                    None
                }
            }
        };

        self.index += 1;

        layout
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
        pos: ScrollMotionTarget,
        mode: DisplayMode,
    ) {
        let mut sb = self.scroll_state.borrow_mut();
        sb.update_contents(contents, pos, mode);
        self.update_edge_indicator(&sb);
    }

    fn scroll(self: &Rc<Self>, fin: Option<CommandResponder>, dx: i32, dy: i32) {
        let mut sb = self.scroll_state.borrow_mut();
        match sb.apply_smooth_scroll(dx, dy) {
            ScrollResult::NoOp => {}
            ScrollResult::Applied => {
                self.update_edge_indicator(&sb);
            }
            ScrollResult::NormalPagination(p) => {
                let (d, smt, pages) = if p == Pagination::Forwards {
                    // If the direction is forwards, only go forwards if there is conceivably
                    // something to move to.

                    let pages = match &self.state.borrow().content {
                        GuiContent::Single(_) => 1,
                        GuiContent::Multiple { next: OffscreenContent::Nothing, .. } => {
                            // Don't bother trying to paginate if there's nothing to paginate to.
                            // Could be jank if the user's preload settings are too low. Oh well
                            // for them.
                            return;
                        }
                        GuiContent::Multiple { visible, .. } => visible.len(),
                    };


                    (Direction::Forwards, ScrollMotionTarget::Start, pages)
                } else {
                    (Direction::Backwards, ScrollMotionTarget::End, 1)
                };

                self.manager_sender
                    .send((ManagerAction::MovePages(d, pages), smt.into(), fin))
                    .expect("Failed to send from Gui to Manager")
            }
        }
    }

    pub(super) fn scroll_down(self: &Rc<Self>, fin: Option<CommandResponder>) {
        self.scroll(fin, 0, *SCROLL_AMOUNT);
    }

    pub(super) fn scroll_up(self: &Rc<Self>, fin: Option<CommandResponder>) {
        self.scroll(fin, 0, -*SCROLL_AMOUNT);
    }

    pub(super) fn scroll_right(self: &Rc<Self>, fin: Option<CommandResponder>) {
        self.scroll(fin, *SCROLL_AMOUNT, 0);
    }

    pub(super) fn scroll_left(self: &Rc<Self>, fin: Option<CommandResponder>) {
        self.scroll(fin, -*SCROLL_AMOUNT, 0);
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

    fn do_continuous_pagination(self: &Rc<Self>, p: Pagination) {
        let d = match p {
            Pagination::Forwards => Direction::Forwards,
            Pagination::Backwards => Direction::Backwards,
        };

        self.manager_sender
            .send((ManagerAction::MovePages(d, 1), ScrollMotionTarget::Continuous(p).into(), None))
            .expect("Failed to send from Gui to Manager")
    }

    pub(super) fn pad_scroll(self: &Rc<Self>, x: f64, y: f64) {
        let mut sb = self.scroll_state.borrow_mut();
        if let Some(p) = sb.pad_scroll(x, y) {
            self.do_continuous_pagination(p);
        }
        self.update_edge_indicator(&sb);
        self.canvas.queue_draw();
    }

    pub(super) fn drag_update(self: &Rc<Self>, x: f64, y: f64) {
        let mut sb = self.scroll_state.borrow_mut();
        if let Some(p) = sb.apply_drag_update(x, y) {
            self.do_continuous_pagination(p);
        }
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
        if let Some(p) = self.scroll_state.borrow_mut().smooth_step() {
            self.do_continuous_pagination(p);
        }

        self.canvas.queue_draw();
        Continue(true)
    }
}
