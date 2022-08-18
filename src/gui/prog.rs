use std::rc::Rc;
use std::time::Duration;

use gtk::traits::{BoxExt, MediaStreamExt, RangeExt, ScaleExt, WidgetExt};
use gtk::MediaStream;

use super::Gui;
use crate::com::AnimatedImage;

#[derive(Debug, Default)]
enum Connection {
    #[default]
    Nothing,
    Animation,
    Video(MediaStream),
}

#[derive(Debug)]
pub(super) struct Progress {
    slider: gtk::Scale,
    time_text: gtk::Label,
    container: gtk::Box,
    spacer: gtk::Box,

    tick_value: f64,
    total: Duration,

    connection: Connection,
}

// We're not really set up for really long times.
// MM:SS.mmm or SS.mmm depending on total duration.
fn format_dur(dur: Duration, total: Duration) -> String {
    let t_seconds = total.as_secs_f32();
    let seconds = dur.as_secs_f32();

    if total >= Duration::from_secs(60) {
        let minutes = (seconds / 60.0).floor() as u32;
        let seconds = seconds % 60.0;
        let t_minutes = (t_seconds / 60.0).floor() as u32;
        let t_seconds = t_seconds % 60.0;

        format!("{minutes}:{seconds:06.3} / {t_minutes}:{t_seconds:06.3}")
    } else if total >= Duration::from_secs(10) {
        format!("{seconds:06.3} / {t_seconds:06.3}")
    } else {
        format!("{seconds:.3} / {t_seconds:.3}")
    }
}

impl Default for Progress {
    fn default() -> Self {
        Self {
            slider: gtk::Scale::with_range(gtk::Orientation::Horizontal, 0.0, 1.0, 1.0),
            time_text: gtk::Label::default(),
            container: gtk::Box::new(gtk::Orientation::Horizontal, 15),
            spacer: gtk::Box::new(gtk::Orientation::Horizontal, 15),

            tick_value: 0.0,
            total: Duration::default(),

            connection: Connection::default(),
        }
    }
}

impl Progress {
    pub(super) fn layout(&mut self, gui: &Rc<Gui>) {
        let g = gui.clone();
        self.slider.connect_change_value(move |_scale, _type, value| {
            let mut s = g.progress.borrow_mut();
            if s.tick_value != value {
                // GTK can give us negative durations.
                let d = Duration::from_secs_f64(f64::max(value, 0.0));

                match &s.connection {
                    Connection::Nothing => {}
                    Connection::Animation => {
                        g.canvas.inner().seek_animation(d);
                    }
                    Connection::Video(ms) => ms.seek(d.as_micros() as i64),
                }

                s.tick(d);
            }

            gtk::Inhibit(false)
        });

        self.container.set_hexpand(true);
        self.container.set_halign(gtk::Align::Fill);
        self.container.set_margin_start(20);
        self.container.set_margin_end(20);

        self.spacer.set_hexpand(true);
        self.spacer.set_halign(gtk::Align::Center);

        self.slider.set_hexpand(true);
        self.slider.set_width_request(100);
        self.slider.set_can_focus(false);

        self.container.append(&self.slider);
        self.container.append(&self.time_text);

        gui.bottom_bar.append(&self.container);
        gui.bottom_bar.append(&self.spacer);

        self.container.hide();
    }

    pub fn hide(&mut self) {
        self.container.hide();
        self.spacer.show();

        std::mem::take(&mut self.connection);
    }

    fn show(&mut self, total: Duration) {
        self.total = total;
        self.slider.set_range(0.0, total.as_secs_f64());
        self.slider.set_value(0.0);
        self.time_text.set_text(&format_dur(Duration::ZERO, total));
        self.tick_value = 0.0;

        self.container.show();
        self.spacer.hide();
    }

    fn tick(&mut self, dur: Duration) {
        let s = dur.as_secs_f64();
        if self.slider.value() != s {
            self.tick_value = s;
            self.slider.set_value(s);
            self.time_text.set_text(&format_dur(dur, self.total));
        }
    }

    pub(super) fn animation_tick(&mut self, dur: Duration) {
        if let Connection::Animation = self.connection {
            self.tick(dur);
        }
    }

    pub(super) fn attach_video(&mut self, v: &gtk::Video, gui: &Rc<Gui>) {
        // We don't need to explicitly drop the connections since they're weak refs.
        std::mem::take(&mut self.connection);

        let ms = v.media_stream().unwrap();

        self.slider.clear_marks();
        self.show(Duration::from_micros(ms.duration() as u64));

        let g = gui.clone();
        ms.connect_duration_notify(move |m| {
            let mut prog = g.progress.borrow_mut();
            prog.show(Duration::from_micros(m.duration() as u64));
        });

        let g = gui.clone();
        ms.connect_timestamp_notify(move |m| {
            let mut prog = g.progress.borrow_mut();
            prog.tick(Duration::from_micros(m.timestamp() as u64));
        });

        self.connection = Connection::Video(ms);
    }

    pub(super) fn attach_animation(&mut self, a: &AnimatedImage) {
        std::mem::take(&mut self.connection);

        self.show(a.dur());

        self.slider.clear_marks();
        for d in a.frames().cumulative_dur.iter().skip(1) {
            self.slider.add_mark(d.as_secs_f64(), gtk::PositionType::Top, None);
        }

        self.connection = Connection::Animation;
    }
}
