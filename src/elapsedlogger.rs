// Was originally adapted from https://github.com/seanmonstar/pretty-env-logger but now bears no
// resemblance.
// Shows time elapsed from application startup and strips out "aw_man::".

use std::time::Instant;

use nu_ansi_term::{Color, Style};
use once_cell::sync::Lazy;
use tracing::{Level, Subscriber};
use tracing_log::NormalizeEvent;
use tracing_subscriber::fmt::format::Writer;
use tracing_subscriber::fmt::{FmtContext, FormatEvent, FormatFields, FormattedFields};
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::EnvFilter;

static START: Lazy<Instant> = Lazy::new(Instant::now);

static PREFIX: &str = concat!(env!("CARGO_CRATE_NAME"), "::");

struct Format {}

impl<S, N> FormatEvent<S, N> for Format
where
    S: Subscriber + for<'a> LookupSpan<'a>,
    N: for<'a> FormatFields<'a> + 'static,
{
    fn format_event(
        &self,
        ctx: &FmtContext<'_, S, N>,
        mut writer: Writer<'_>,
        event: &tracing::Event<'_>,
    ) -> std::fmt::Result {
        let normalized = event.normalized_metadata();
        let meta = normalized.as_ref().unwrap_or_else(|| event.metadata());
        let target = meta.target();
        let target = target.strip_prefix(PREFIX).map_or(target, shrink_target);

        let now = Instant::now();
        let dur = now.duration_since(*START);
        let seconds = dur.as_secs();
        let ms = dur.as_millis() % 1000;

        write!(writer, "{seconds:04}.{ms:03} ")?;

        if writer.has_ansi_escapes() {
            let styled = {
                match *meta.level() {
                    Level::TRACE => Color::Purple.bold().paint("TRACE"),
                    Level::DEBUG => Color::Blue.bold().paint("DEBUG"),
                    Level::INFO => Color::Green.bold().paint("INFO "),
                    Level::WARN => Color::Yellow.bold().paint("WARN "),
                    Level::ERROR => Color::Red.bold().paint("ERROR"),
                }
            };
            write!(writer, "{styled} ")?;
        } else {
            write!(writer, "{: <5} ", *meta.level())?
        }

        let dimmed = if writer.has_ansi_escapes() { Style::new().dimmed() } else { Style::new() };

        if let Some(scope) = ctx.event_scope() {
            let bold = if writer.has_ansi_escapes() { Style::new().bold() } else { Style::new() };

            let mut seen = false;

            for span in scope.from_root() {
                let name = span.metadata().name();
                if !name.is_empty() {
                    if seen {
                        write!(writer, "{}", dimmed.paint(":"))?;
                    }
                    write!(writer, "{}", bold.paint(span.metadata().name()))?;
                }

                seen = true;

                if let Some(fields) = &span.extensions().get::<FormattedFields<N>>() {
                    if !fields.is_empty() {
                        write!(writer, "{}{}{}", bold.paint("{"), fields, bold.paint("}"))?;
                    }
                }
            }

            if seen {
                writer.write_char(' ')?;
            }
        };

        write!(writer, "{} ", dimmed.paint(target))?;

        ctx.format_fields(writer.by_ref(), event)?;
        writeln!(writer)
    }
}


pub fn init_logging() {
    Lazy::force(&START); // Inititalize the start time.

    let filter_layer =
        EnvFilter::builder().with_default_directive(Level::INFO.into()).from_env_lossy();

    let fmt_layer = tracing_subscriber::fmt::layer().event_format(Format {});
    tracing_subscriber::registry()
        .with(filter_layer)
        .with(fmt_layer)
        // .with(ErrorLayer::default())
        .init();
}

// Strips all but the last two modules.
fn shrink_target(target: &str) -> &str {
    if let Some(x) = target.rfind("::") {
        if let Some(x) = target[0..x].rfind("::") {
            return &target[x + 2..];
        }
    }
    target
}
