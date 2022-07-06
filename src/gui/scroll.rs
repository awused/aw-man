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
    CommandResponder, DebugIgnore, Direction, DisplayMode, Fit, ManagerAction, Pagination, Res,
    ScrollMotionTarget, TargetRes,
};
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
    Multiple {
        // prev and next are true if we can continue to scroll into the next page.
        prev: bool,
        visible: Vec<Res>,
        next: bool,
    },
}

impl ScrollContents {
    // Find the total res from fitting all visible images using the current fit strategy and the
    // current scroll bounds.
    //
    // Continuous vertical or horizontal scrolling might lead to unusual behaviour if some images
    // are of different resolutions.
    fn fit(&self, target_res: TargetRes, mode: DisplayMode) -> (Res, Res) {
        match self {
            ScrollContents::Single(r) => {
                let fitted = r.fit_inside(target_res);
                let bounds = (
                    fitted.w.saturating_sub(target_res.res.w),
                    fitted.h.saturating_sub(target_res.res.h),
                )
                    .into();
                (fitted, bounds)
            }
            ScrollContents::Multiple { visible, .. } => match mode {
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
                    let bounds = (
                        fitted.w.saturating_sub(target_res.res.w),
                        // Subtract 1 from first.h to avoid scrolling one pixel past the bottom
                        min(fitted.h.saturating_sub(target_res.res.h), first.h.saturating_sub(1)),
                    )
                        .into();
                    (fitted, bounds)
                }
            },
        }
    }

    // Used when moving back using Jump/PreviousPage/etc.
    fn first_element_end_position(&self, target_res: TargetRes, mode: DisplayMode) -> (u32, u32) {
        let fitted = match self {
            ScrollContents::Single(r) => r.fit_inside(target_res),
            ScrollContents::Multiple { visible, .. } => match mode {
                DisplayMode::Single => unreachable!(),
                DisplayMode::VerticalStrip => visible[0].fit_inside(target_res),
            },
        };

        (
            fitted.w.saturating_sub(target_res.res.w),
            fitted.h.saturating_sub(target_res.res.h),
        )
    }

    const fn more_forward(&self) -> bool {
        match self {
            ScrollContents::Single(_) => false,
            ScrollContents::Multiple { next, .. } => *next,
        }
    }

    const fn more_backward(&self) -> bool {
        match self {
            ScrollContents::Single(_) => false,
            ScrollContents::Multiple { prev, .. } => *prev,
        }
    }

    fn count(&self) -> usize {
        match self {
            ScrollContents::Single(_) => 1,
            ScrollContents::Multiple { visible, .. } => visible.len(),
        }
    }
}

enum Edges {
    Top,
    Bottom,
    // TODO -- None/Left/Right
    Neither,
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
    x: u32,
    y: u32,
    // Used to store the current scroll position when swapping between views where the image is
    // fully visible and bounds become 0.
    saved_positions: (f64, f64),

    motion: Motion,

    // The maximum and minimum scroll bounds for the currently visible images.
    bounds: Res,
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
    MustPaginate(Pagination),
    ContinuousPagination(Pagination),
}

impl ScrollResult {
    const fn should_apply(self) -> bool {
        match self {
            Self::NoOp | Self::MustPaginate(_) => false,
            Self::Applied | Self::ContinuousPagination(_) => true,
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
            bounds: (0, 0).into(),
            contents: ScrollContents::Single((0, 0).into()),
            mode: DisplayMode::Single,
            fitted_res: (0, 0).into(),
            target_res: (0, 0, Fit::Container).into(),

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

        let old_contents = std::mem::replace(&mut self.contents, contents);
        let old_bounds = self.bounds;

        self.mode = mode;
        (self.fitted_res, self.bounds) = self.contents.fit(self.target_res, mode);

        match pos {
            ScrollMotionTarget::Start => {
                self.x = 0;
                self.y = 0;
                self.saved_positions = (0.0, 0.0);
            }
            ScrollMotionTarget::End => {
                (self.x, self.y) =
                    self.contents.first_element_end_position(self.target_res, self.mode);
                // This seems weird, but it's probably closer to my intention most of the time.
                // If the current page isn't scrolling when the user reaches it this will treat it
                // the same as if I paged down.
                // If it is scrolling, this will not matter and it will stay wherever it is placed.
                self.saved_positions = (0.0, 0.0);
            }
            ScrollMotionTarget::Maintain => {
                // Single -> Multiple should remaing unchanged
                // Multiple -> Single should clamp
                match (&self.contents, old_contents) {
                    (ScrollContents::Single(_), ScrollContents::Multiple { .. })
                    | (ScrollContents::Multiple { .. }, ScrollContents::Single(_)) => {
                        self.x = min(self.x, self.bounds.w);
                        self.y = min(self.y, self.bounds.h);
                    }
                    (ScrollContents::Single(_), ScrollContents::Single(_))
                    | (ScrollContents::Multiple { .. }, ScrollContents::Multiple { .. }) => {
                        self.readjust_scroll_in_place(old_bounds)
                    }
                }
            }
            ScrollMotionTarget::Continuous(Pagination::Forwards) => {
                todo!()
            }
            ScrollMotionTarget::Continuous(Pagination::Backwards) => {
                todo!()
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
        self.target_res = target_res;

        let old_bounds = self.bounds;
        (self.fitted_res, self.bounds) = self.contents.fit(self.target_res, self.mode);

        self.readjust_scroll_in_place(old_bounds);
    }

    pub fn readjust_scroll_in_place(&mut self, old_bounds: Res) {
        // TODO -- should this be based entirely on the first element when continuously scrolling?
        // Being at the "end" of the first element feels like it should stay there, but it's a
        // pretty minor concern all told.
        if old_bounds.w > 0 {
            self.saved_positions.0 = min(self.x, old_bounds.w) as f64 / old_bounds.w as f64;
        }
        if old_bounds.h > 0 {
            self.saved_positions.1 = min(self.y, old_bounds.h) as f64 / old_bounds.h as f64;
        }

        self.x = (self.saved_positions.0 * self.bounds.w as f64).round() as u32;
        self.y = (self.saved_positions.1 * self.bounds.h as f64).round() as u32;
    }

    // In continuous mode the smooth scrolling callback will be responsible for switching pages.
    fn apply_smooth_scroll(&mut self, dx: i32, dy: i32) -> ScrollResult {
        let (tx, ty) = if let Motion::Smooth { x: (_, tx), y: (_, ty), .. } = self.motion {
            (tx + dx, ty + dy)
        } else {
            (self.x as i32 + dx, self.y as i32 + dy)
        };

        let r = self.check_pagination(dx, dy, tx, ty);
        if !r.should_apply() {
            return r;
        }

        if let Motion::Smooth { x, y, start, step, .. } = &mut self.motion {
            *x = (self.x as i32, tx);
            *y = (self.y as i32, ty);
            *start = *step;
        } else {
            let scroll_start = Instant::now();
            let tick_id = ManuallyDrop::new((self.add_tick_callback)());

            self.motion = Motion::Smooth {
                x: (self.x as i32, tx),
                y: (self.y as i32, ty),
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
                    if (tx - dx <= 0 && dx <= 0) || (tx - dx >= self.bounds.w as i32 && dx >= 0) {
                        ScrollResult::NoOp
                    } else {
                        ScrollResult::Applied
                    }
                }
                1.. => {
                    // Positive = Down = Forwards
                    if self.contents.more_forward() {
                        ScrollResult::ContinuousPagination(Pagination::Forwards)
                    } else if self.y == self.bounds.h || ty > self.bounds.h as i32 + *SCROLL_AMOUNT
                    {
                        ScrollResult::MustPaginate(Pagination::Forwards)
                    } else {
                        ScrollResult::Applied
                    }
                }
                _ => {
                    if self.contents.more_backward() {
                        ScrollResult::ContinuousPagination(Pagination::Backwards)
                    } else if self.y == 0 || ty < -*SCROLL_AMOUNT {
                        ScrollResult::MustPaginate(Pagination::Backwards)
                    } else {
                        ScrollResult::Applied
                    }
                }
            }
        } else {
            unimplemented!()
        }
    }

    fn pad_scroll(&mut self, x: f64, y: f64) {
        let dx = (x * *SCROLL_AMOUNT as f64).round() as i32;
        let dy = (y * *SCROLL_AMOUNT as f64).round() as i32;

        self.motion = Motion::Stationary;
        // TODO -- handle dragging against the edge of an image. Clamp drag_offset to the value
        // actually applied then return the remainder or a bounding indicator.
        // TODO -- continuous mode scrolling increment/decrement page
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

        // TODO -- continuous mode scrolling increment/decrement page

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

    fn smooth_step(&mut self) -> ScrollResult {
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

        let tx = x.0 + dx;
        let ty = y.0 + dy;

        let r = self.check_pagination(dx, dy, tx, ty);

        match r {
            // We can return early if it's a no-op, but that's usually true for the first step with
            // 0 ms
            ScrollResult::NoOp | ScrollResult::Applied => (),
            ScrollResult::MustPaginate(_) => {
                if (self.mode.vertical_pagination() && x.0 == x.1)
                    || (!self.mode.vertical_pagination() && y.0 == y.1)
                {
                    trace!("Reached end of page while smooth scrolling, ending early.");
                    self.motion = Motion::Stationary;
                    // Don't return early, still apply clamped values;
                }
            }
            ScrollResult::ContinuousPagination(pagination) => {
                debug!("Should perform continuous paginate {pagination:?}")
            }
        }

        self.x = tx.clamp(0, self.bounds.w as i32) as u32;
        self.y = ty.clamp(0, self.bounds.h as i32) as u32;

        trace!(
            "Smooth scroll step: {scale} {:?} {x:?} {y:?} {} {}",
            now - start,
            self.x,
            self.y,
        );

        if scale == 1.0 {
            // We're done if we're in Single mode or if we're in Continuous mode and there's
            // nowhere to scroll.
            match self.contents {
                ScrollContents::Single(_) => self.motion = Motion::Stationary,
                ScrollContents::Multiple { .. } => todo!(),
            }
        }

        r
    }

    fn touched_edges(&self) -> Edges {
        // TODO -- other modes.
        assert!(self.mode.vertical_pagination());
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
            ScrollContents::Multiple { prev, next, .. } => {
                if ty <= 0 && !prev {
                    Edges::Top
                } else if ty >= self.bounds.h as i32 && !next {
                    Edges::Bottom
                } else {
                    Edges::Neither
                }
            }
        }
    }

    pub(super) const fn layout_iter(&self) -> LayoutIterator {
        // Only supports vertical continuous scrolling
        let v_off = if let ScrollContents::Multiple { prev: true, .. } = self.contents {
            (self.y as i32).saturating_neg()
        } else {
            self.target_res.res.h.saturating_sub(self.fitted_res.h) as i32 / 2 - self.y as i32
        };

        let upper_left = (
            self.target_res.res.w.saturating_sub(self.fitted_res.w) as i32 / 2 - self.x as i32,
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

                    // If this is thinner than the other elements, center it.
                    // Might cause weird jumping around if some elements are wider than the screen,
                    // not much to be done there.
                    ofx += (self.state.fitted_res.w - res.w) as i32 / 2;

                    // For now only vertical continuous scrolling is allowed.
                    // This could be modified for horizontal or dual-page scrolling.
                    self.current_offset.1 += res.h as i32;

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

    pub(super) fn scroll_down(self: &Rc<Self>, fin: Option<CommandResponder>) {
        let mut sb = self.scroll_state.borrow_mut();
        match sb.apply_smooth_scroll(0, *SCROLL_AMOUNT) {
            ScrollResult::NoOp => {}
            ScrollResult::Applied => {
                self.update_edge_indicator(&sb);
            }
            ScrollResult::MustPaginate(_) => self
                .manager_sender
                .send((
                    ManagerAction::MovePages(Direction::Forwards, 1),
                    ScrollMotionTarget::Start.into(),
                    fin,
                ))
                .expect("Failed to send from Gui to Manager"),
            // This shouldn't need to do anything, the tick callback should get to it, but it'll be
            // faster.
            ScrollResult::ContinuousPagination(_) => {
                self.update_edge_indicator(&sb);
                todo!()
            }
        }
    }

    pub(super) fn scroll_up(self: &Rc<Self>, fin: Option<CommandResponder>) {
        let mut sb = self.scroll_state.borrow_mut();
        match sb.apply_smooth_scroll(0, -*SCROLL_AMOUNT) {
            ScrollResult::NoOp => {}
            ScrollResult::Applied => {
                self.update_edge_indicator(&sb);
            }
            ScrollResult::MustPaginate(_) => self
                .manager_sender
                .send((
                    ManagerAction::MovePages(Direction::Backwards, 1),
                    ScrollMotionTarget::End.into(),
                    fin,
                ))
                .expect("Failed to send from Gui to Manager"),
            // This shouldn't need to do anything, the tick callback should get to it, but it'll be
            // faster.
            ScrollResult::ContinuousPagination(_) => {
                self.update_edge_indicator(&sb);
                todo!()
            }
        }
    }

    pub(super) fn scroll_right(self: &Rc<Self>, _fin: Option<CommandResponder>) {
        self.scroll_state.borrow_mut().apply_smooth_scroll(*SCROLL_AMOUNT, 0);
    }

    pub(super) fn scroll_left(self: &Rc<Self>, _fin: Option<CommandResponder>) {
        self.scroll_state.borrow_mut().apply_smooth_scroll(-*SCROLL_AMOUNT, 0);
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
        if let ScrollResult::ContinuousPagination(_d) = self.scroll_state.borrow_mut().smooth_step()
        {
            todo!();
        }
        self.canvas.queue_draw();
        Continue(true)
    }
}
