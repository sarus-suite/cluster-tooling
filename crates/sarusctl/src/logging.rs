use nu_ansi_term::AnsiGenericString;
use tracing::{Event, Subscriber};
use tracing_subscriber::{
    self,
    filter::{LevelFilter, filter_fn},
    fmt::{
        self, FmtContext,
        format::{FmtSpan, FormatEvent, FormatFields, Writer},
        time::{FormatTime, SystemTime},
    },
    layer::{Layer, SubscriberExt},
    registry::LookupSpan,
    util::SubscriberInitExt,
};

struct ProfileFormatter;

impl<S, N> FormatEvent<S, N> for ProfileFormatter
where
    S: Subscriber + for<'a> LookupSpan<'a>,
    N: for<'a> FormatFields<'a> + 'static,
{
    fn format_event(
        &self,
        ctx: &FmtContext<'_, S, N>,
        mut writer: Writer<'_>,
        event: &Event<'_>,
    ) -> std::fmt::Result {
        use nu_ansi_term::{Color, Style};
        let ansi = writer.has_ansi_escapes();
        let profile_string = if ansi {
            Color::Cyan.paint("PROFILE")
        } else {
            AnsiGenericString::from("PROFILE ")
        };
        let span_style = if ansi {
            Style::new().bold()
        } else {
            Style::new()
        };

        SystemTime.format_time(&mut writer)?;

        write!(writer, " {} ", profile_string)?;

        if let Some(scope) = ctx.event_scope() {
            for span in scope.from_root() {
                // Print only the span name. Do not retrieve
                // FormattedFields<N> from span.extensions().
                write!(writer, "{}:", span_style.paint(span.name()))?;
            }
        }

        write!(writer, " {}: ", event.metadata().target())?;

        // For a synthesized CLOSE event this prints:
        // close time.busy=... time.idle=...
        ctx.field_format().format_fields(writer.by_ref(), event)?;

        writeln!(writer)
    }
}

pub(crate) fn init_tracing(verbosity: u8, profile: bool) {
    let level = match verbosity {
        0 => LevelFilter::WARN,
        1 => LevelFilter::INFO,
        2 => LevelFilter::DEBUG,
        _ => LevelFilter::TRACE,
    };

    let log_layer = fmt::layer()
        .with_span_events(FmtSpan::CLOSE)
        .with_filter(level);

    let profile_layer = profile.then(|| {
        tracing_subscriber::fmt::layer()
            .with_span_events(FmtSpan::CLOSE)
            .event_format(ProfileFormatter)
            .with_filter(LevelFilter::TRACE)
            .with_filter(filter_fn(|metadata| {
                metadata.is_span()
                    && (metadata.target().starts_with("sarusctl")
                        || metadata.target().starts_with("raster"))
            }))
    });

    tracing_subscriber::registry()
        .with(log_layer)
        .with(profile_layer)
        .init();
}
