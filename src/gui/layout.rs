use std::cmp::{max, min};
use std::mem::ManuallyDrop;
use std::rc::{Rc, Weak};
use std::time::{Duration, Instant};

use gtk::glib::{self, Continue};
use gtk::prelude::{WidgetExt, WidgetExtManual};
use gtk::TickCallbackId;
use once_cell::sync::Lazy;

use super::Gui;
use crate::com::{
    CommandResponder, DebugIgnore, Direction, DisplayMode, Fit, GuiContent, LayoutCount,
    ManagerAction, OffscreenContent, Pagination, Res, ScrollMotionTarget, TargetRes,
};
use crate::config::CONFIG;

static SCROLL_AMOUNT: Lazy<i32> = Lazy::new(|| CONFIG.scroll_amount.get() as i32);

static SCROLL_DURATION: Duration = Duration::from_millis(166);

pub static APPROX_SCROLL_STEP: Lazy<i32> = Lazy::new(|| {
    // This could try to determine the actual framerate, but it is unlikely to matter much.
    let frames = SCROLL_DURATION.as_millis() as f64 / 16.667;
    (*SCROLL_AMOUNT as f64 / frames).ceil() as i32
});

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
pub(super) enum LayoutContents {
    Single(Res),
    Dual(Res, Res),
    Strip {
        // Whatever the "current" page is, which is not necessarily visible[0]
        current_index: usize,
        visible: Vec<Res>,
    },
}

impl LayoutContents {
    // Find the total res from fitting all visible images using the current fit strategy and the
    // current scroll bounds.
    //
    // Continuous vertical or horizontal scrolling might lead to unusual behaviour if some images
    // are of different resolutions.
    fn fit(&self, target_res: TargetRes, mode: DisplayMode) -> (Res, Res, Rect) {
        let from_fitted = |fitted: Res| {
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
        };

        match self {
            Self::Single(r) => from_fitted(r.fit_inside(target_res)),
            Self::Dual(first, second) => {
                let first = first.fit_inside(target_res);
                let second = second.fit_inside(target_res);
                from_fitted((first.w + second.w, max(first.h, second.h)).into())
            }
            Self::Strip { current_index, visible, .. } => match mode {
                DisplayMode::Single | DisplayMode::DualPage | DisplayMode::DualPageReversed => {
                    unreachable!()
                }
                DisplayMode::VerticalStrip => {
                    let first = visible[*current_index].fit_inside(target_res);
                    let mut max_x = first.w;
                    let mut sum_y = first.h;

                    for v in &visible[(current_index + 1)..] {
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

                    // We don't consider completely off-screen elements.
                    let top: u32 =
                        visible[0..*current_index].iter().map(|v| v.fit_inside(target_res).h).sum();
                    let top = -(top as i32);

                    let true_bounds = Rect {
                        top,
                        left: 0,
                        bottom: sum_y.saturating_sub(target_res.res.h) as i32,
                        right: pagination_bounds.w as i32,
                    };


                    (fitted, pagination_bounds, true_bounds)
                }
                DisplayMode::HorizontalStrip => {
                    let first = visible[*current_index].fit_inside(target_res);
                    let mut sum_x = first.w;
                    let mut max_y = first.h;

                    for v in &visible[(current_index + 1)..] {
                        let t = v.fit_inside(target_res);

                        sum_x += t.w;
                        max_y = max(max_y, t.h);
                    }

                    let fitted: Res = (sum_x, max_y).into();

                    // Subtract 1 from first.w to avoid scrolling one pixel past the right
                    let pagination_bounds: Res = (
                        min(fitted.w.saturating_sub(target_res.res.w), first.w.saturating_sub(1)),
                        fitted.h.saturating_sub(target_res.res.h),
                    )
                        .into();

                    // We don't consider completely off-screen elements.
                    let left: u32 =
                        visible[0..*current_index].iter().map(|v| v.fit_inside(target_res).w).sum();
                    let left = -(left as i32);

                    let true_bounds = Rect {
                        top: 0,
                        left,
                        bottom: pagination_bounds.h as i32,
                        right: sum_x.saturating_sub(target_res.res.w) as i32,
                    };


                    (fitted, pagination_bounds, true_bounds)
                }
            },
        }
    }

    // Used when moving back using Jump/PreviousPage/etc.
    fn first_element_end_position(&self, target_res: TargetRes) -> (u32, u32) {
        let fitted = self.first_res(target_res);

        (
            fitted.w.saturating_sub(target_res.res.w),
            fitted.h.saturating_sub(target_res.res.h),
        )
    }

    fn first_res(&self, target_res: TargetRes) -> Res {
        match self {
            Self::Single(r) => r.fit_inside(target_res),
            Self::Dual(first, second) => {
                let first = first.fit_inside(target_res);
                let second = second.fit_inside(target_res);
                (first.w + second.w, max(first.h, second.h)).into()
            }
            Self::Strip { current_index, visible, .. } => {
                visible[*current_index].fit_inside(target_res)
            }
        }
    }
}

pub(super) enum Edge {
    Top,
    Bottom,
    Left,
    Right,
}

impl Edge {
    const fn icon(self) -> &'static str {
        match self {
            Self::Top => "🡅",
            Self::Bottom => "🡇",
            Self::Left => "🡄",
            Self::Right => "🡆",
        }
    }
}

#[derive(Debug, Default)]
struct Rect {
    top: i32,
    bottom: i32,
    left: i32,
    right: i32,
}

// Eventually include videos and animations
// This struct only tracks what is immediately necessary for scrolling and laying out currently
// visible pages.
#[derive(Debug)]
pub(super) struct LayoutManager {
    // The current visible offsets of the upper left corner of the viewport relative to the upper
    // left corner of the singular "current page" plus letterboxing.
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
    // modes can temporarily fall outside of these bounds, but this will result in the current page
    // changing. If self.y = page_bounds.h, then that means there is one row of pixels visible
    // for the current image.
    page_bounds: Res,

    contents: LayoutContents,
    mode: DisplayMode,
    fitted_res: Res,
    target_res: TargetRes,

    add_tick_callback: DebugIgnore<Box<dyn Fn() -> TickCallbackId>>,
}


#[derive(Debug, Eq, PartialEq, Copy, Clone)]
enum ScrollResult {
    NoOp,
    Applied,
    // The precise meaning varies depending on scroll type.
    // Hard pagination for ScrollUp/Down/Left/Right
    // Soft pagination otherwise, including steps of smooth scrolling from Scroll commands.
    Pagination(Pagination),
}

impl ScrollResult {
    const fn should_apply(self) -> bool {
        match self {
            Self::NoOp | Self::Pagination(_) => false,
            Self::Applied => true,
        }
    }
}

impl LayoutManager {
    pub(super) fn new(gui: Weak<Gui>) -> Self {
        let add_tick_callback: DebugIgnore<Box<dyn Fn() -> TickCallbackId>> =
            DebugIgnore(Box::new(move || gui.upgrade().unwrap().add_tick_callback()));

        Self {
            x: 0,
            y: 0,
            saved_positions: (0.0, 0.0),

            motion: Motion::Stationary,

            mode: DisplayMode::Single,
            target_res: (0, 0, Fit::Container, DisplayMode::Single).into(),
            contents: LayoutContents::Single((0, 0).into()),

            fitted_res: (0, 0).into(),
            page_bounds: (0, 0).into(),
            true_bounds: Rect::default(),

            add_tick_callback,
        }
    }

    fn update_contents(
        &mut self,
        contents: LayoutContents,
        pos: ScrollMotionTarget,
        mode: DisplayMode,
    ) {
        // Cancel any ongoing scrolling except when continuously scrolling and paginating.
        if !pos.continue_current_scroll() {
            self.motion = Motion::Stationary;
        }

        let old_first_res = self.contents.first_res(self.target_res);
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
                let end = self.contents.first_element_end_position(self.target_res);
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
                self.y = (self.y + dy as i32).clamp(0, self.page_bounds.h as i32);

                // This shouldn't actually matter unless we're going from a state the user couldn't
                // normally scroll to to this page. Like if they explicitly paged down past where
                // they normally could then started to scroll up.
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
            LayoutContents::Single((0, 0).into()),
            ScrollMotionTarget::Start,
            self.mode,
        )
    }

    fn update_container(&mut self, target_res: TargetRes) {
        // We resized, stop any scrolling now.
        self.motion = Motion::Stationary;

        let old_bounds = self.page_bounds;
        let old_first_res = self.contents.first_res(self.target_res);

        self.target_res = target_res;
        (self.fitted_res, self.page_bounds, self.true_bounds) =
            self.contents.fit(self.target_res, self.mode);

        self.readjust_scroll_in_place(old_first_res, old_bounds);
    }

    pub fn readjust_scroll_in_place(&mut self, old_res: Res, old_bounds: Res) {
        let new_res = self.contents.first_res(self.target_res);

        if old_bounds.w > 0 {
            self.saved_positions.0 = min(self.x, old_res.w as i32) as f64 / old_res.w as f64;
        }
        if old_bounds.h > 0 {
            self.saved_positions.1 = min(self.y, old_res.h as i32) as f64 / old_res.h as f64;
        }

        self.x = ((self.saved_positions.0 * new_res.w as f64).round() as i32)
            .clamp(0, self.page_bounds.w as i32);
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
                        ScrollResult::Pagination(Pagination::Forwards)
                    } else {
                        ScrollResult::Applied
                    }
                }
                _ => {
                    if self.true_bounds.top == 0 && (self.y == 0 || ty < -*SCROLL_AMOUNT) {
                        ScrollResult::Pagination(Pagination::Backwards)
                    } else {
                        ScrollResult::Applied
                    }
                }
            }
        } else {
            match dx {
                0 => {
                    // ty - dy is not necessarily equal to self.y
                    if (ty - dy <= 0 && dy <= 0)
                        || (ty - dy >= self.page_bounds.h as i32 && dy >= 0)
                    {
                        ScrollResult::NoOp
                    } else {
                        ScrollResult::Applied
                    }
                }
                1.. => {
                    // Positive = Right = Forwards
                    if self.true_bounds.right == self.page_bounds.w as i32
                        && (self.x == self.true_bounds.right
                            || tx > self.true_bounds.right + *SCROLL_AMOUNT)
                    {
                        ScrollResult::Pagination(Pagination::Forwards)
                    } else {
                        ScrollResult::Applied
                    }
                }
                _ => {
                    if self.true_bounds.left == 0 && (self.x == 0 || tx < -*SCROLL_AMOUNT) {
                        ScrollResult::Pagination(Pagination::Backwards)
                    } else {
                        ScrollResult::Applied
                    }
                }
            }
        }
    }

    fn pad_scroll(&mut self, x: f64, y: f64) -> ScrollResult {
        let dx = (x * *SCROLL_AMOUNT as f64).round() as i32;
        let dy = (y * *SCROLL_AMOUNT as f64).round() as i32;

        self.motion = Motion::Stationary;

        self.apply_delta(dx, dy).2
    }

    pub(super) fn start_drag(&mut self) {
        self.motion = Motion::Dragging { offset: (0, 0) };
    }

    fn apply_drag_update(&mut self, ofx: f64, ofy: f64) -> ScrollResult {
        let drag_offset = if let Motion::Dragging { offset } = &self.motion {
            offset
        } else {
            // This may happen if the user has multiple scroll devices. Not worth handling.
            debug!("Got dragging event outside of dragging scroll mode.");
            return ScrollResult::NoOp;
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
    fn apply_delta(&mut self, dx: i32, dy: i32) -> (i32, i32, ScrollResult) {
        let tx = self.x + dx;
        let ty = self.y + dy;

        let (old_x, old_y) = (self.x, self.y);

        self.x = tx.clamp(self.true_bounds.left, self.true_bounds.right);
        self.y = ty.clamp(self.true_bounds.top, self.true_bounds.bottom);

        // TODO -- handle the case where the user has started paginating and, before it completes,
        // returns to the "current" page. This can potentially happen multiple times and cause
        // strange behaviour, but nothing will permanently break.
        let p = if self.x == old_x && self.y == old_y {
            ScrollResult::NoOp
        } else if (self.x < 0 && old_x >= 0) || (self.y < 0 && old_y >= 0) {
            ScrollResult::Pagination(Pagination::Backwards)
        } else if (self.x > self.page_bounds.w as i32 && old_x <= self.page_bounds.w as i32)
            || (self.y > self.page_bounds.h as i32 && old_y <= self.page_bounds.h as i32)
        {
            ScrollResult::Pagination(Pagination::Forwards)
        } else {
            ScrollResult::Applied
        };

        (tx - self.x, ty - self.y, p)
    }

    // Returns a direction for continuous scrolling, if necessary
    fn smooth_step(&mut self) -> ScrollResult {
        let now = Instant::now();

        let (x, y, start) = if let Motion::Smooth { x, y, start, ref mut step, .. } = self.motion {
            *step = now;
            (x, y, start)
        } else {
            unreachable!();
        };

        let scale =
            f64::min((now - start).as_micros() as f64 / SCROLL_DURATION.as_micros() as f64, 1.0);

        let dx = ((x.1 - x.0) as f64 * scale).round() as i32;
        let dy = ((y.1 - y.0) as f64 * scale).round() as i32;

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

    const fn touched_edge(&self) -> Option<Edge> {
        if self.mode.vertical_pagination() {
            if self.page_bounds.h == 0 {
                return None;
            }

            let ty = match self.motion {
                Motion::Smooth { y: (_, ty), .. } => ty,
                Motion::Stationary | Motion::Dragging { .. } => self.y,
            };

            if ty <= 0 && self.true_bounds.top == 0 {
                Some(Edge::Top)
            } else if ty >= self.true_bounds.bottom
                && self.true_bounds.bottom == self.page_bounds.h as i32
            {
                Some(Edge::Bottom)
            } else {
                None
            }
        } else {
            if self.page_bounds.w == 0 {
                return None;
            }

            let tx = match self.motion {
                Motion::Smooth { x: (_, tx), .. } => tx,
                Motion::Stationary | Motion::Dragging { .. } => self.x,
            };

            if tx <= 0 && self.true_bounds.left == 0 {
                Some(Edge::Left)
            } else if tx >= self.true_bounds.right
                && self.true_bounds.right == self.page_bounds.w as i32
            {
                Some(Edge::Right)
            } else {
                None
            }
        }
    }

    // TODO -- For all snap commands, should they continue on to the next page or just remain
    // no-ops?
    fn snap_to_top(&mut self) -> ScrollResult {
        if self.y >= 0 {
            let result = if self.y == 0 { ScrollResult::NoOp } else { ScrollResult::Applied };
            self.y = 0;
            self.saved_positions.1 = 0.0;
            match &mut self.motion {
                Motion::Stationary => (),
                Motion::Smooth { y, .. } => {
                    y.0 = 0;
                    y.1 = 0;
                }
                Motion::Dragging { .. } => self.motion = Motion::Stationary,
            }
            result
        } else {
            // We should already be in the process of paginating. So it should be safe to set y
            // = -(previous element fitted height) but this is difficult to
            // test. Regardless, the chance of a user hitting this is extremely
            // low (takes a few hundred microseconds to resolve at the high end)
            // so it's not necessary to handle.
            debug!("Dropped snap to top during pagination");
            ScrollResult::NoOp
        }
    }

    fn snap_to_bottom(&mut self) -> ScrollResult {
        let end = self.contents.first_element_end_position(self.target_res);
        let end_y = end.1 as i32;

        if self.y > self.page_bounds.h as i32 {
            debug!("Dropped snap to bottom during pagination");
            ScrollResult::NoOp
        } else if self.y > end_y {
            // We're in vertical strip mode and we're already past the point where the bottom
            // of the current image is visible, but not yet at the point where the image is no
            // longer visible.
            debug!("Ignoring snap to bottom when already past the bottom");
            ScrollResult::NoOp
        } else {
            let result = if self.y == end_y { ScrollResult::NoOp } else { ScrollResult::Applied };
            self.y = end_y;
            self.saved_positions.1 = 1.0;

            match &mut self.motion {
                Motion::Stationary => (),
                Motion::Smooth { y, .. } => {
                    y.0 = end_y;
                    y.1 = end_y;
                }
                Motion::Dragging { .. } => self.motion = Motion::Stationary,
            }

            result
        }
    }

    fn snap_to_left(&mut self) -> ScrollResult {
        if self.x >= 0 {
            let result = if self.x == 0 { ScrollResult::NoOp } else { ScrollResult::Applied };
            self.x = 0;
            self.saved_positions.0 = 0.0;
            match &mut self.motion {
                Motion::Stationary => (),
                Motion::Smooth { x, .. } => {
                    x.0 = 0;
                    x.1 = 0;
                }
                Motion::Dragging { .. } => self.motion = Motion::Stationary,
            }
            result
        } else {
            debug!("Dropped snap to left during pagination");
            ScrollResult::NoOp
        }
    }

    fn snap_to_right(&mut self) -> ScrollResult {
        let end = self.contents.first_element_end_position(self.target_res);
        let end_x = end.0 as i32;

        if self.x > self.page_bounds.w as i32 {
            debug!("Dropped snap to right during pagination");
            ScrollResult::NoOp
        } else if self.x > end_x {
            // We're in horizontal strip mode and we're already past the point where the right side
            // of the current image is visible, but not yet at the point where the image is no
            // longer visible.
            debug!("Ignoring snap to right when already past the right");
            ScrollResult::NoOp
        } else {
            let result = if self.x == end_x { ScrollResult::NoOp } else { ScrollResult::Applied };
            self.x = end_x;
            self.saved_positions.0 = 1.0;

            match &mut self.motion {
                Motion::Stationary => (),
                Motion::Smooth { x, .. } => {
                    x.0 = end_x;
                    x.1 = end_x;
                }
                Motion::Dragging { .. } => self.motion = Motion::Stationary,
            }

            result
        }
    }

    pub(super) const fn layout_iter(&self) -> LayoutIterator {
        let upper_left = match self.contents {
            LayoutContents::Strip { current_index: 1.., .. } => {
                if self.mode.vertical_pagination() {
                    (
                        self.target_res.res.w.saturating_sub(self.fitted_res.w) as i32 / 2 - self.x
                            + self.true_bounds.left,
                        self.y.saturating_neg() + self.true_bounds.top,
                    )
                } else {
                    (
                        self.x.saturating_neg() + self.true_bounds.left,
                        self.target_res.res.h.saturating_sub(self.fitted_res.h) as i32 / 2 - self.y
                            + self.true_bounds.top,
                    )
                }
            }
            LayoutContents::Single(_)
            | LayoutContents::Dual { .. }
            | LayoutContents::Strip { .. } => (
                self.target_res.res.w.saturating_sub(self.fitted_res.w) as i32 / 2 - self.x
                    + self.true_bounds.left,
                self.target_res.res.h.saturating_sub(self.fitted_res.h) as i32 / 2 - self.y
                    + self.true_bounds.top,
            ),
        };

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
    state: &'a LayoutManager,
    upper_left: (i32, i32),
    current_offset: (i32, i32),
}

impl<'a> Iterator for LayoutIterator<'a> {
    type Item = (i32, i32, Res);

    fn next(&mut self) -> Option<Self::Item> {
        let layout = match &self.state.contents {
            LayoutContents::Single(r) => {
                let res = r.fit_inside(self.state.target_res);
                if self.index == 0 {
                    (self.upper_left.0, self.upper_left.1, res)
                } else {
                    return None;
                }
            }
            LayoutContents::Dual(first, second) => {
                let v = match self.index {
                    0 => first,
                    1 => second,
                    _ => return None,
                };

                let res = v.fit_inside(self.state.target_res);
                let (mut ofx, mut ofy) = (
                    self.upper_left.0 + self.current_offset.0,
                    self.upper_left.1 + self.current_offset.1,
                );

                ofy += (self.state.fitted_res.h.saturating_sub(res.h)) as i32 / 2;

                match self.state.mode {
                    DisplayMode::DualPage => {
                        self.current_offset.0 += res.w as i32;
                    }
                    DisplayMode::DualPageReversed => {
                        if self.index == 0 {
                            ofx = self.upper_left.0 + self.state.fitted_res.w as i32 - res.w as i32
                        }
                    }
                    DisplayMode::Single
                    | DisplayMode::VerticalStrip
                    | DisplayMode::HorizontalStrip => unreachable!(),
                }

                (ofx, ofy, res)
            }
            LayoutContents::Strip { visible, .. } => {
                let v = visible.get(self.index)?;
                let res = v.fit_inside(self.state.target_res);
                let (mut ofx, mut ofy) = (
                    self.upper_left.0 + self.current_offset.0,
                    self.upper_left.1 + self.current_offset.1,
                );

                if self.state.mode.vertical_pagination() {
                    // If this is thinner than the other elements, center it.
                    // Might cause weird jumping around if some elements are wider than the
                    // screen, not much to be done there.
                    ofx += (self.state.fitted_res.w.saturating_sub(res.w)) as i32 / 2;

                    self.current_offset.1 += res.h as i32;
                } else {
                    ofy += (self.state.fitted_res.h.saturating_sub(res.h)) as i32 / 2;

                    self.current_offset.0 += res.w as i32;
                }

                (ofx, ofy, res)
            }
        };

        self.index += 1;

        Some(layout)
    }
}

impl Gui {
    pub(super) fn update_scroll_container(self: &Rc<Self>, target_res: TargetRes) {
        let mut sb = self.layout_manager.borrow_mut();
        sb.update_container(target_res);
        self.update_edge_indicator(&sb);
    }

    pub(super) fn zero_scroll(self: &Rc<Self>) {
        let mut sb = self.layout_manager.borrow_mut();
        sb.zero();
        self.update_edge_indicator(&sb);
    }

    pub(super) fn update_scroll_contents(
        self: &Rc<Self>,
        contents: LayoutContents,
        pos: ScrollMotionTarget,
        mode: DisplayMode,
    ) {
        let mut sb = self.layout_manager.borrow_mut();
        sb.update_contents(contents, pos, mode);
        self.update_edge_indicator(&sb);
    }

    fn scroll(self: &Rc<Self>, fin: Option<CommandResponder>, dx: i32, dy: i32) {
        let mut sb = self.layout_manager.borrow_mut();
        match sb.apply_smooth_scroll(dx, dy) {
            ScrollResult::NoOp => {}
            ScrollResult::Applied => {
                self.update_edge_indicator(&sb);
            }
            ScrollResult::Pagination(p) => {
                self.last_action.set(Some(Instant::now()));

                let (d, smt, pages) = if p == Pagination::Forwards {
                    let pages = match &self.state.borrow().content {
                        GuiContent::Single { .. } => 1,
                        GuiContent::Dual { next: OffscreenContent::Nothing, .. }
                        | GuiContent::Strip { next: OffscreenContent::Nothing, .. } => {
                            // Don't bother trying to paginate if there's nothing to paginate to.
                            // Could be jank if the user's preload settings are too low. Oh well.
                            return;
                        }
                        GuiContent::Dual { visible, .. } => visible.count(),
                        GuiContent::Strip { current_index, visible, .. } => {
                            visible.len() - current_index
                        }
                    };


                    (Direction::Forwards, ScrollMotionTarget::Start, pages)
                } else {
                    let pages = match &self.state.borrow().content {
                        GuiContent::Single { .. } => 1,
                        GuiContent::Strip { prev: OffscreenContent::Nothing, .. } => {
                            // Don't bother trying to paginate if there's nothing to paginate to.
                            // Could be jank if the user's preload settings are too low. Oh well.
                            return;
                        }
                        GuiContent::Dual { prev, .. } => match prev {
                            OffscreenContent::Nothing => return,
                            OffscreenContent::LayoutCompatible(LayoutCount::TwoOrMore) => 2,
                            OffscreenContent::LayoutIncompatible
                            | OffscreenContent::LayoutCompatible(_)
                            | OffscreenContent::Unknown => 1,
                        },
                        GuiContent::Strip { current_index, .. } => current_index + 1,
                    };

                    (Direction::Backwards, ScrollMotionTarget::End, pages)
                };

                self.send_manager((ManagerAction::MovePages(d, pages), smt.into(), fin));
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

    fn simple_scroll<F>(self: &Rc<Self>, scroll_fn: F, x: f64, y: f64)
    where
        F: FnOnce(&mut LayoutManager, f64, f64) -> ScrollResult,
    {
        let mut sb = self.layout_manager.borrow_mut();
        match scroll_fn(&mut sb, x, y) {
            ScrollResult::NoOp => return,
            ScrollResult::Applied => (),
            ScrollResult::Pagination(p) => self.do_continuous_pagination(p),
        }
        self.update_edge_indicator(&sb);
        self.canvas.queue_draw();
    }

    pub(super) fn pad_scroll(self: &Rc<Self>, x: f64, y: f64) {
        self.simple_scroll(LayoutManager::pad_scroll, x, y);
    }

    pub(super) fn drag_update(self: &Rc<Self>, x: f64, y: f64) {
        self.simple_scroll(LayoutManager::apply_drag_update, x, y);
    }

    pub(super) fn snap(self: &Rc<Self>, edge: Edge, _fin: Option<CommandResponder>) {
        let mut sb = self.layout_manager.borrow_mut();
        let result = match edge {
            Edge::Top => sb.snap_to_top(),
            Edge::Bottom => sb.snap_to_bottom(),
            Edge::Left => sb.snap_to_left(),
            Edge::Right => sb.snap_to_right(),
        };
        match result {
            ScrollResult::NoOp => return,
            ScrollResult::Applied => (),
            ScrollResult::Pagination(_) => unreachable!(),
        }
        self.update_edge_indicator(&sb);
        self.canvas.queue_draw();
    }

    fn add_tick_callback(self: Rc<Self>) -> TickCallbackId {
        trace!("Beginning smooth scrolling");
        let g = self.clone();
        self.canvas.add_tick_callback(move |_canvas, _clock| g.tick_callback())
    }

    fn update_edge_indicator(self: &Rc<Self>, scroll: &LayoutManager) {
        let icon = scroll.touched_edge().map_or("", Edge::icon);
        let g = self.clone();
        glib::idle_add_local_once(move || {
            if g.edge_indicator.text().as_str() != icon {
                g.edge_indicator.set_text(icon);
            }
        });
    }

    fn tick_callback(self: &Rc<Self>) -> Continue {
        match self.layout_manager.borrow_mut().smooth_step() {
            ScrollResult::NoOp => return Continue(true),
            ScrollResult::Applied => (),
            ScrollResult::Pagination(p) => self.do_continuous_pagination(p),
        }

        self.canvas.queue_draw();
        Continue(true)
    }

    fn do_continuous_pagination(self: &Rc<Self>, p: Pagination) {
        let d = match p {
            Pagination::Forwards => Direction::Forwards,
            Pagination::Backwards => Direction::Backwards,
        };

        self.send_manager((
            ManagerAction::MovePages(d, 1),
            ScrollMotionTarget::Continuous(p).into(),
            None,
        ));
    }
}
