//! Time to first token and turn time, measured twice on the same ask: once by
//! watching the stream, once by calling `finish()` and looking only at the end.
//!
//! The two numbers are different questions. `finish()` drains the same events —
//! it is not a second request — but a host that never looks at an event cannot
//! see the first one arrive, so it has no time-to-first-token at all. This
//! benchmark runs both against a live provider and reports the distribution, so
//! the cost of *not* streaming is a number, not an assumption.
//!
//! Every delta is classified as it arrives — text, reasoning, or something else
//! — because a model that spends its whole budget thinking has a first token and
//! no answer, and that is a latency fact worth seeing rather than hiding.
//!
//! ```sh
//! OPENROUTER_API_KEY=sk-or-... \
//! BENCH_ENDPOINT=https://openrouter.ai/api/v1 \
//! BENCH_MODEL=deepseek/deepseek-v4-flash-0731 \
//!   cargo run -p norte-bench-latency -- --rounds 5
//! ```

use std::error::Error;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures_util::StreamExt as _;
use norte_bench_support::report::{Align, Report};
use norte_bench_support::{Mode, Run, captured as new_capture};
use norte_core::{
    Capabilities, Engine, EngineBuilder, EngineEvent, InMemoryBlobStore, InMemoryStore, Message,
    Modality, Role, StreamDelta, StreamingGranularity, Usage,
};
use serde::Serialize;

const DEFAULT_PROMPT: &str = "Write a four-line haiku about latency.";

const USAGE: &str = "\
norte-bench-latency — time to first token and turn time, live

USAGE:
    norte-bench-latency [OPTIONS]

OPTIONS:
    --rounds <n>    rounds per mode (default 5)
    --prompt <s>    the ask (default: a four-line haiku about latency)
    --report <p>    write the markdown report to <p>
    --jsonl <p>     write the JSONL trail to <p>
    --help, -h      print this help

Live: needs a key and a route in the environment.
  OPENROUTER_API_KEY (or BENCH_KEY_ENV), BENCH_ENDPOINT, BENCH_MODEL, BENCH_PROVIDER,
  BENCH_PAUSE_MS.
";

/// One measured turn.
#[allow(clippy::struct_field_names)]
#[derive(Serialize, Clone)]
struct Round {
    mode: String,
    round: u64,
    ttft_ms: Option<f64>,
    total_ms: f64,
    text_deltas: u64,
    reasoning_deltas: u64,
    other_deltas: u64,
    bytes: u64,
    output_tokens: Option<u64>,
    cache_read: Option<u64>,
    tokens_per_second: Option<f64>,
    cost: Option<f64>,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let Some(mut run) = Run::start(
        "latency",
        USAGE,
        Mode::Live,
        Path::new(env!("CARGO_MANIFEST_DIR")),
    )?
    else {
        return Ok(());
    };

    let rounds = run.cli().option_u64("rounds")?.unwrap_or(5).max(1);
    let prompt = run
        .cli()
        .option("prompt")
        .unwrap_or(DEFAULT_PROMPT)
        .to_owned();
    let route = run
        .route()
        .ok_or("the latency benchmark is live and needs a route")?
        .clone();
    let options = route.turn_options(512);

    let mut records = Vec::new();
    for round in 0..rounds {
        let engine = build_engine(&route)?;
        records.push(
            measure_streaming(&engine, &prompt, options.clone(), round, route.pause()).await?,
        );
        let engine = build_engine(&route)?;
        records
            .push(measure_buffered(&engine, &prompt, options.clone(), round, route.pause()).await?);
    }

    let report = render(&route, &records, &prompt, rounds);
    run.table("rounds", &CSV_HEADERS, csv_rows(&records));
    for record in &records {
        run.record("round", record);
    }
    run.finish(report)?;
    Ok(())
}

fn build_engine(route: &norte_bench_support::Route) -> Result<Engine, Box<dyn Error>> {
    let adapter = route.openai(text_capabilities())?;
    // The pause is taken by the benchmark, before the clock starts, so it does
    // not land in either latency figure.
    let probe = norte_bench_support::RecordingOpenAi::new(adapter, new_capture(), Duration::ZERO);
    Ok(EngineBuilder::new()
        .store(Arc::new(InMemoryStore::new()))
        .blobs(Arc::new(InMemoryBlobStore::new()))
        .model(Arc::new(probe))
        .build()?)
}

fn text_capabilities() -> Capabilities {
    Capabilities::builder()
        .input(Modality::Text)
        .output(Modality::Text)
        .streaming(StreamingGranularity::Delta)
        .build()
}

/// Consumes the stream event by event, so the first delta has a timestamp.
async fn measure_streaming(
    engine: &Engine,
    prompt: &str,
    options: norte_core::TurnOptions,
    round: u64,
    pause: Duration,
) -> Result<Round, Box<dyn Error>> {
    let session = engine.create().await?;
    if !pause.is_zero() {
        tokio::time::sleep(pause).await;
    }
    let started = Instant::now();
    let mut stream = engine
        .turn(session.id)
        .input(Message::user(prompt))
        .options(options)
        .start();
    let mut ttft: Option<f64> = None;
    let mut text_deltas = 0_u64;
    let mut reasoning_deltas = 0_u64;
    let mut other_deltas = 0_u64;
    let mut bytes = 0_u64;
    while let Some(event) = stream.next().await {
        if let EngineEvent::StreamDelta { delta, .. } = event? {
            ttft.get_or_insert_with(|| millis(started.elapsed()));
            match delta {
                StreamDelta::Text { text, .. } => {
                    text_deltas += 1;
                    bytes += u64::try_from(text.len()).unwrap_or(0);
                }
                StreamDelta::Reasoning { .. } => reasoning_deltas += 1,
                _ => other_deltas += 1,
            }
        }
    }
    stream.finish().await?;
    let total = started.elapsed();
    let usage = usage(engine, session.id).await;
    Ok(assemble(
        "streaming",
        round,
        &Counts {
            ttft,
            total,
            text_deltas,
            reasoning_deltas,
            other_deltas,
            bytes,
        },
        &usage,
    ))
}

/// Calls `finish()` and never looks at an event.
async fn measure_buffered(
    engine: &Engine,
    prompt: &str,
    options: norte_core::TurnOptions,
    round: u64,
    pause: Duration,
) -> Result<Round, Box<dyn Error>> {
    let session = engine.create().await?;
    if !pause.is_zero() {
        tokio::time::sleep(pause).await;
    }
    let started = Instant::now();
    engine
        .turn(session.id)
        .input(Message::user(prompt))
        .options(options)
        .start()
        .finish()
        .await?;
    let total = started.elapsed();
    let usage = usage(engine, session.id).await;
    Ok(assemble(
        "buffered",
        round,
        &Counts {
            ttft: None,
            total,
            text_deltas: 0,
            reasoning_deltas: 0,
            other_deltas: 0,
            bytes: 0,
        },
        &usage,
    ))
}

/// What the stream showed, before it is joined with the provider's counters.
struct Counts {
    ttft: Option<f64>,
    total: Duration,
    text_deltas: u64,
    reasoning_deltas: u64,
    other_deltas: u64,
    bytes: u64,
}

fn assemble(mode: &str, round: u64, counts: &Counts, usage: &Usage) -> Round {
    let total_ms = millis(counts.total);
    let output_tokens = usage.output_tokens;
    let seconds = counts.total.as_secs_f64();
    let tokens_per_second = output_tokens.and_then(|tokens| {
        if seconds > 0.0 {
            Some(norte_bench_support::as_f64(tokens) / seconds)
        } else {
            None
        }
    });
    Round {
        mode: mode.to_owned(),
        round,
        ttft_ms: counts.ttft,
        total_ms,
        text_deltas: counts.text_deltas,
        reasoning_deltas: counts.reasoning_deltas,
        other_deltas: counts.other_deltas,
        bytes: counts.bytes,
        output_tokens,
        cache_read: usage.cache_read_tokens,
        tokens_per_second,
        cost: norte_bench_support::cost_of(usage),
    }
}

/// The usage the provider reported for the turn just committed.
async fn usage(engine: &Engine, session: norte_core::SessionId) -> Usage {
    engine
        .history(session)
        .await
        .ok()
        .and_then(|history| {
            history
                .iter()
                .rev()
                .find(|message| message.role == Role::Assistant)
                .and_then(|message| message.origin.clone())
                .map(|origin| origin.usage)
        })
        .unwrap_or_default()
}

fn millis(elapsed: Duration) -> f64 {
    elapsed.as_secs_f64() * 1000.0
}

fn format_ms(value: f64) -> String {
    format!("{value:.1}")
}

fn format_opt_ms(value: Option<f64>) -> String {
    value.map_or_else(|| "—".to_owned(), format_ms)
}

/// Milliseconds are timing, never money; a truncating cast is what is wanted.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn as_u64(value: f64) -> u64 {
    value.max(0.0) as u64
}

const CSV_HEADERS: [&str; 13] = [
    "mode",
    "round",
    "ttft_ms",
    "total_ms",
    "text_deltas",
    "reasoning_deltas",
    "other_deltas",
    "bytes",
    "output_tokens",
    "cache_read",
    "tokens_per_second",
    "cost",
    "note",
];

fn csv_rows(records: &[Round]) -> Vec<Vec<String>> {
    records
        .iter()
        .map(|record| {
            let note = if record.mode == "streaming" && record.text_deltas == 0 {
                "no text delta arrived".to_owned()
            } else {
                String::new()
            };
            vec![
                record.mode.clone(),
                record.round.to_string(),
                format_opt_ms(record.ttft_ms),
                format_ms(record.total_ms),
                record.text_deltas.to_string(),
                record.reasoning_deltas.to_string(),
                record.other_deltas.to_string(),
                record.bytes.to_string(),
                record
                    .output_tokens
                    .map_or_else(|| "—".to_owned(), |v| v.to_string()),
                record
                    .cache_read
                    .map_or_else(|| "—".to_owned(), |v| v.to_string()),
                record
                    .tokens_per_second
                    .map_or_else(|| "—".to_owned(), |v| format!("{v:.1}")),
                record
                    .cost
                    .map_or_else(|| "—".to_owned(), |v| format!("{v:.6}")),
                note,
            ]
        })
        .collect()
}

fn percentile(sorted: &[f64], per_mille: u64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let last = sorted.len() - 1;
    let index = usize::try_from(u64::try_from(last).unwrap_or(0).saturating_mul(per_mille) / 1000)
        .unwrap_or(last);
    sorted[index.min(last)]
}

#[allow(clippy::too_many_lines)]
fn render(
    route: &norte_bench_support::Route,
    records: &[Round],
    prompt: &str,
    rounds: u64,
) -> Report {
    let mut report = Report::new("Latency");
    report.paragraph(
        "One ask, measured twice: watching the stream (time to first token) and calling \
         `finish()` without reading an event (turn time). `finish()` is not a second \
         request — it drains the same stream — but a host that does not read events has \
         no TTFT to report.",
    );
    report.section("Run");
    report.kv(&[
        ("route", route.summary()),
        (
            "pause",
            format!(
                "{} ms before each request, outside the timer",
                route.pause_ms
            ),
        ),
        ("rounds per mode", rounds.to_string()),
        ("prompt", prompt.to_owned()),
        ("modes", "streaming, buffered".to_owned()),
    ]);

    report.section("Summary");
    let modes = ["streaming", "buffered"];
    let rows: Vec<Vec<String>> = modes
        .iter()
        .map(|mode| {
            let subset: Vec<&Round> = records.iter().filter(|r| r.mode == *mode).collect();
            let mut totals: Vec<f64> = subset.iter().map(|r| r.total_ms).collect();
            totals.sort_by(f64::total_cmp);
            let mut ttfts: Vec<f64> = subset.iter().filter_map(|r| r.ttft_ms).collect();
            ttfts.sort_by(f64::total_cmp);
            let text: u64 = subset.iter().map(|r| r.text_deltas).sum();
            let reasoning: u64 = subset.iter().map(|r| r.reasoning_deltas).sum();
            let output: u64 = subset.iter().filter_map(|r| r.output_tokens).sum();
            let cache_read: u64 = subset.iter().filter_map(|r| r.cache_read).sum();
            let cost: f64 = subset.iter().filter_map(|r| r.cost).sum();
            vec![
                (*mode).to_owned(),
                subset.len().to_string(),
                format_opt_ms(ttfts.first().copied()),
                format_opt_ms(ttfts.get(ttfts.len() / 2).copied()),
                format_ms(*totals.first().unwrap_or(&0.0)),
                format_ms(percentile(&totals, 500)),
                format_ms(*totals.last().unwrap_or(&0.0)),
                text.to_string(),
                reasoning.to_string(),
                output.to_string(),
                cache_read.to_string(),
                format!("{cost:.6}"),
            ]
        })
        .collect();
    report.table(
        &[
            "mode",
            "rounds",
            "ttft min",
            "ttft p50",
            "total min",
            "total p50",
            "total max",
            "text deltas",
            "reasoning",
            "output tokens",
            "cache read",
            "cost",
        ],
        &[
            Align::Left,
            Align::Right,
            Align::Right,
            Align::Right,
            Align::Right,
            Align::Right,
            Align::Right,
            Align::Right,
            Align::Right,
            Align::Right,
            Align::Right,
            Align::Right,
        ],
        &rows,
    );

    for mode in modes {
        let values: Vec<u64> = records
            .iter()
            .filter(|r| r.mode == mode)
            .map(|r| as_u64(r.total_ms))
            .collect();
        report.sparkline(&format!("{mode} turn time (ms)"), &values);
    }

    report.section("Per round");
    report.table(
        &[
            "mode",
            "round",
            "ttft ms",
            "total ms",
            "text",
            "reason",
            "other",
            "bytes",
            "out tokens",
            "cache read",
            "tok/s",
            "cost",
            "note",
        ],
        &[
            Align::Left,
            Align::Right,
            Align::Right,
            Align::Right,
            Align::Right,
            Align::Right,
            Align::Right,
            Align::Right,
            Align::Right,
            Align::Right,
            Align::Right,
            Align::Right,
            Align::Left,
        ],
        &csv_rows(records),
    );

    report.section("Reading");
    let fastest = records
        .iter()
        .map(|record| record.total_ms)
        .reduce(f64::min)
        .unwrap_or(0.0);
    let slowest = records
        .iter()
        .map(|record| record.total_ms)
        .reduce(f64::max)
        .unwrap_or(0.0);
    report.paragraph(&format!(
        "A turn here takes from {} to {}. The streaming host sees its first token at \
         `ttft`; the buffered host waits the whole turn before it can decide anything. \
         A round with `note = no text delta arrived` spent the whole output budget before \
         producing any answer text — latency the caller feels as nothing happening.",
        format_ms(fastest),
        format_ms(slowest)
    ));
    report
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_percentile_picks_an_element() {
        let sorted = vec![1.0, 2.0, 3.0, 4.0, 5.0];
        assert!(percentile(&sorted, 500) >= 3.0);
        assert!((percentile(&sorted, 0) - 1.0).abs() < f64::EPSILON);
        assert!((percentile(&sorted, 1000) - 5.0).abs() < f64::EPSILON);
    }
}
