mod cache;
mod cli;
mod context;
mod exec;
mod format;
mod git;
mod hashing;
mod io;
mod lint;
mod query;
mod utils;

use crate::{
    cache::Cache,
    cli::{Args, Command},
    context::{CONTEXT, Context},
};
use clap::Parser as _;
use color_eyre::eyre;
use std::{env, thread::available_parallelism};
use tokio::sync::Semaphore;
use tracing::{Event, Subscriber};
use tracing_error::ErrorLayer;
use tracing_indicatif::{
    IndicatifLayer,
    filter::{IndicatifFilter, hide_indicatif_span_fields},
    style::ProgressStyle,
};
use tracing_subscriber::{
    filter::EnvFilter,
    fmt::{
        FmtContext, FormatEvent, FormatFields,
        format::{Compact, DefaultFields, Format, Pretty, Writer},
    },
    layer::{Layer as _, SubscriberExt},
    registry::LookupSpan,
    util::SubscriberInitExt as _,
};

// TODO: Currently I'm excluding files from consideration simply if they are on `master`, because
// they should've been formatted/linted/whatever successfully there. But I should not apply this
// shortcut if any dependent config files have changed from `master`! For example, if I change how
// formatting works, or add a new lint, I want _all_ files to be considered, because the rules have
// changed since `master`.

#[tokio::main]
async fn main() -> eyre::Result<()> {
    let args = Args::parse();

    color_eyre::install()?;
    init_tracing(&args)?;

    let cache = Cache::new().await?;
    let file_permits = Semaphore::new(100);
    let process_permits = Semaphore::new(usize::from(available_parallelism()?));

    CONTEXT.get_or_init(move || Context {
        cache,
        file_permits,
        process_permits,
    });

    match &args.command {
        Command::Format(args) => format::run(args).await,
        Command::Lint(args) => lint::run(args).await,
        Command::Query(args) => query::run(args).await,
    }
}

fn init_tracing(args: &Args) -> eyre::Result<()> {
    let indicatif_layer = IndicatifLayer::new()
        .with_span_field_formatter(hide_indicatif_span_fields(DefaultFields::new()))
        .with_progress_style(ProgressStyle::with_template(
            "{span_child_prefix}{span_name}{{{span_fields}}",
        )?);

    let event_format = if args.verbose_expanded == 0 {
        BeFormatEvent::Compact(tracing_subscriber::fmt::format().compact())
    } else {
        BeFormatEvent::Pretty(tracing_subscriber::fmt::format().pretty())
    };

    let env_filter = match args.verbose + args.verbose_expanded {
        0 => "warn,be=info",
        1 => "info,be=debug",
        2 => "info,be=trace",
        3 => "trace",
        4.. => {
            tracing::warn!("max verbosity is 3");
            "trace"
        }
    };

    let fmt_layer = tracing_subscriber::fmt::layer()
        .event_format(event_format)
        .with_writer(indicatif_layer.get_stderr_writer())
        .with_filter(EnvFilter::new(
            env::var("RUST_LOG").as_deref().unwrap_or(env_filter),
        ));

    tracing_subscriber::registry()
        .with(fmt_layer)
        .with(indicatif_layer.with_filter(IndicatifFilter::new(false)))
        .with(ErrorLayer::default())
        .init();

    Ok(())
}

enum BeFormatEvent {
    Compact(Format<Compact>),
    Pretty(Format<Pretty>),
}

impl<S, N> FormatEvent<S, N> for BeFormatEvent
where
    S: Subscriber + for<'a> LookupSpan<'a>,
    N: for<'a> FormatFields<'a> + 'static,
{
    fn format_event(
        &self,
        ctx: &FmtContext<'_, S, N>,
        writer: Writer<'_>,
        event: &Event<'_>,
    ) -> std::fmt::Result {
        match self {
            Self::Compact(f) => f.format_event(ctx, writer, event),
            Self::Pretty(f) => f.format_event(ctx, writer, event),
        }
    }
}
