//! Shared reporting, audit, route and measurement support for the Norte
//! benchmarks.
//!
//! A benchmark measures a real engine and writes down what happened so a number
//! can be audited later: a markdown report for a reader, a JSONL trail for a
//! machine, and a run manifest that pins the commit, the toolchain, the route
//! and a redacted environment. This crate holds what every benchmark needs and
//! nothing that is specific to one measurement.
//!
//! The intended layout of a benchmark `main` is:
//!
//! 1. [`Run::start`] parses the command line, resolves the route when the
//!    benchmark is live, and prints `--help` without touching the network.
//! 2. The measurement records every request, attempt or case with
//!    [`Run::record`].
//! 3. [`Report`] is built for the reader and handed to [`Run::finish`], which
//!    writes the markdown, the JSONL trail and the manifest together.

pub mod artifacts;
pub mod cli;
pub mod csv;
pub mod model;
pub mod report;
pub mod route;
pub mod run;
pub mod time;
pub mod usage;

pub use artifacts::{
    Artifacts, Jsonl, JsonlRecord, Manifest, default_path, rustc_version, slug, write_json,
};
pub use cli::Cli;
pub use csv::Csv;
pub use model::{Captured, RecordingOpenAi, RequestRecord, captured};
pub use report::{Align, Report};
pub use route::Route;
pub use run::{Mode, Run};
pub use time::{as_f64, duration, money, money_opt, percent, sparkline, tokens};
pub use usage::{Counters, cost_of};
