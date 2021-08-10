use std::cmp::min;
use std::rc::Rc;
use std::time::Instant;

use gtk::prelude::WidgetExt;
use once_cell::sync::Lazy;

use super::Gui;
use crate::com::{CommandResponder, Direction, ManagerAction, Res};
use crate::config::CONFIG;

static SCROLL_AMOUNT: Lazy<u32> = Lazy::new(|| CONFIG.scroll_amount);


#[derive(Debug)]
pub(super) struct ScrollState {
    // The current visible offsets of the upper left corner of the viewport relative to the upper
    // left corner of the displayed content plus letterboxing.
    pub(super) x: u32,
    pub(super) y: u32,
    // The target offsets during a smooth scroll operation. These are used as the "real" values
    // when the user scrolls again before the first operation completes.
    // When not scrolling these are the same as (x,y).
    tx: u32,
    ty: u32,
    // The maximum and minimum scroll bounds for the current image.
    bounds: Res,
    contents: Res,
    container: Res,
    // tick_callback:
}

impl Default for ScrollState {
    fn default() -> Self {
        Self {
            x: 0,
            y: 0,
            tx: 0,
            ty: 0,
            bounds: (0, 0).into(),
            contents: (0, 0).into(),
            container: (0, 0).into(),
        }
    }
}

#[derive(Debug, Eq, PartialEq, Copy, Clone)]
pub(super) enum ScrollPos {
    Start,
    End,
    Maintain,
}

impl ScrollState {
    pub(super) fn update_contents(&mut self, fitted_res: Res, pos: ScrollPos) {
        self.contents = fitted_res;
        let old_bounds = self.bounds;

        self.bounds = (
            self.contents.w.saturating_sub(self.container.w),
            self.contents.h.saturating_sub(self.container.h),
        )
            .into();


        match pos {
            ScrollPos::Start => {
                self.x = 0;
                self.y = 0;
            }
            ScrollPos::End => {
                self.x = self.bounds.w;
                self.y = self.bounds.h;
            }
            ScrollPos::Maintain => {
                let xpercent = if old_bounds.w > 0 {
                    self.tx as f64 / old_bounds.w as f64
                } else {
                    0.0
                };
                let ypercent = if old_bounds.h > 0 {
                    self.ty as f64 / old_bounds.h as f64
                } else {
                    0.0
                };

                self.x = (xpercent * self.bounds.w as f64).round() as u32;
                self.y = (ypercent * self.bounds.h as f64).round() as u32;
            }
        }

        self.tx = self.x;
        self.ty = self.y;
        // cancel self.tick_callback
    }

    pub(super) fn zero(&mut self) {
        self.update_contents((0, 0).into(), ScrollPos::Start)
    }

    pub(super) fn update_container(&mut self, container_res: Res) {
        self.container = container_res;
        self.update_contents(self.contents, ScrollPos::Maintain);
    }

    // These return false when no scrolling was done because we were already at the edge.
    fn scroll_down(&mut self) -> bool {
        if self.ty == self.bounds.h {
            return false;
        }

        self.ty = min(self.ty + *SCROLL_AMOUNT, self.bounds.h);
        // TODO -- smooth scrolling
        self.y = self.ty;
        true
    }

    fn scroll_up(&mut self) -> bool {
        if self.ty == 0 {
            return false;
        }

        self.ty = self.ty.saturating_sub(*SCROLL_AMOUNT);
        // TODO -- smooth scrolling
        self.y = self.ty;
        true
    }

    fn scroll_right(&mut self) -> bool {
        if self.tx == self.bounds.w {
            return false;
        }

        self.tx = min(self.tx + *SCROLL_AMOUNT, self.bounds.w);
        // TODO -- smooth scrolling
        self.x = self.tx;
        true
    }

    fn scroll_left(&mut self) -> bool {
        if self.tx == 0 {
            return false;
        }

        self.tx = self.tx.saturating_sub(*SCROLL_AMOUNT);

        // TODO -- smooth scrolling
        self.x = self.tx;
        true
    }

    fn continuous_scroll(&mut self, x: f64, y: f64) {
        let dx = (x * *SCROLL_AMOUNT as f64).round() as i32;
        let dy = (y * *SCROLL_AMOUNT as f64).round() as i32;

        if dx >= 0 {
            self.tx = min(self.tx + dx as u32, self.bounds.w);
        } else {
            self.tx = self.tx.saturating_sub(dx.unsigned_abs());
        }

        if dy >= 0 {
            self.ty = min(self.ty + dy as u32, self.bounds.h);
        } else {
            self.ty = self.ty.saturating_sub(dy.unsigned_abs());
        }

        self.x = self.tx;
        self.y = self.ty;
    }
}

impl Gui {
    pub(super) fn scroll_down(self: &Rc<Self>, fin: Option<CommandResponder>) {
        if self.scroll_state.borrow_mut().scroll_down() {
            // TODO -- this shouldn't be necessary once smooth scrolling is enabled.
            self.canvas.queue_draw();
        } else {
            self.scroll_motion_target.set(ScrollPos::Start);
            self.manager_sender
                .send((ManagerAction::MovePages(Direction::Forwards, 1), fin))
                .expect("Failed to send from Gui to Manager")
        }
    }

    pub(super) fn scroll_up(self: &Rc<Self>, fin: Option<CommandResponder>) {
        if self.scroll_state.borrow_mut().scroll_up() {
            // TODO -- this shouldn't be necessary once smooth scrolling is enabled.
            self.canvas.queue_draw();
        } else {
            self.scroll_motion_target.set(ScrollPos::End);
            self.manager_sender
                .send((ManagerAction::MovePages(Direction::Backwards, 1), fin))
                .expect("Failed to send from Gui to Manager")
        }
    }

    pub(super) fn scroll_right(self: &Rc<Self>, _fin: Option<CommandResponder>) {
        if self.scroll_state.borrow_mut().scroll_right() {
            // TODO -- this shouldn't be necessary once smooth scrolling is enabled.
            self.canvas.queue_draw();
        }
    }

    pub(super) fn scroll_left(self: &Rc<Self>, _fin: Option<CommandResponder>) {
        if self.scroll_state.borrow_mut().scroll_left() {
            // TODO -- this shouldn't be necessary once smooth scrolling is enabled.
            self.canvas.queue_draw();
        }
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

    pub(super) fn continuous_scroll(self: &Rc<Self>, x: f64, y: f64) {
        self.scroll_state.borrow_mut().continuous_scroll(x, y);
        self.canvas.queue_draw();
    }
}
