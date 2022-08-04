// Adapted from https://github.com/seanmonstar/pretty-env-logger
// Shows time elapsed from application startup and strips out "aw_man::".

/*
Copyright (c) 2017 Sean McArthur

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in
all copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN
THE SOFTWARE.
*/

use std::cmp::min;
use std::fmt;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use env_logger::fmt::Color;
use log::Level;
use once_cell::sync::Lazy;

static START: Lazy<Instant> = Lazy::new(Instant::now);

pub fn init_logging() {
    Lazy::force(&START); // Inititalize the start time.

    env_logger::Builder::from_default_env()
        .format(|f, record| {
            use std::io::Write;
            let target = record.target();
            let target = target.strip_prefix("aw_man::").unwrap_or(target);
            let target = shrink_target(target);
            let max_width = max_target_width(target);

            let mut style = f.style();
            let level = match record.level() {
                Level::Trace => style.set_color(Color::Magenta).value("TRACE"),
                Level::Debug => style.set_color(Color::Blue).value("DEBUG"),
                Level::Info => style.set_color(Color::Green).value("INFO "),
                Level::Warn => style.set_color(Color::Yellow).value("WARN "),
                Level::Error => style.set_color(Color::Red).value("ERROR"),
            };

            let mut style = f.style();
            let target = style.set_bold(true).value(Padded { value: target, width: max_width });

            let now = Instant::now();
            let dur = now.duration_since(*START);
            let seconds = dur.as_secs();
            let ms = dur.as_millis() % 1000;

            writeln!(f, " {:04}.{:03} {} {} > {}", seconds, ms, level, target, record.args(),)
        })
        .init();
}

struct Padded<T> {
    value: T,
    width: usize,
}

impl<T: fmt::Display> fmt::Display for Padded<T> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{: <width$}", self.value, width = self.width)
    }
}

static MAX_MODULE_WIDTH: AtomicUsize = AtomicUsize::new(0);
const MAX_WIDTH: usize = 10;

// Strips all but the last two modules.
fn shrink_target(target: &str) -> &str {
    if let Some(x) = target.rfind("::") {
        if let Some(x) = target[0..x].rfind("::") {
            return &target[x + 2..];
        }
    }
    target
}

fn max_target_width(target: &str) -> usize {
    let max_width = MAX_MODULE_WIDTH.load(Ordering::Relaxed);
    if max_width < target.len() {
        let newlen = min(target.len(), MAX_WIDTH);
        MAX_MODULE_WIDTH.store(newlen, Ordering::Relaxed);
        target.len()
    } else {
        max_width
    }
}
