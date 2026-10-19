//! The lifecycle every benchmark shares: parse, measure, write the artifacts.

use std::error::Error;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::json;

use crate::artifacts::{
    Artifacts, Jsonl, JsonlRecord, Manifest, commit, default_path, redact_env, rustc_version, slug,
    write_text,
};
use crate::cli::Cli;
use crate::csv::{Csv, write_csv};
use crate::report::Report;
use crate::route::Route;

/// Whether a benchmark needs a route.
#[derive(Debug, Clone, Copy)]
pub enum Mode {
    /// No provider is called; the run is deterministic and can run in CI.
    Offline,
    /// A provider is called; the route comes from the environment.
    Live,
}

/// One benchmark run: its arguments, its route and the records it leaves.
pub struct Run {
    benchmark: &'static str,
    started: DateTime<Utc>,
    argv: Vec<String>,
    cli: Cli,
    dir: PathBuf,
    route: Option<Route>,
    records: Vec<JsonlRecord>,
    tables: Vec<(String, Csv)>,
}

impl Run {
    /// Parses the command line and, when the benchmark is live, the route.
    ///
    /// Returns `None` when `--help` was asked for, after printing `usage`.
    ///
    /// # Errors
    ///
    /// Returns a message for a bad argument, or a live route that is missing
    /// from the environment.
    pub fn start(
        benchmark: &'static str,
        usage: &str,
        mode: Mode,
        dir: &Path,
    ) -> Result<Option<Self>, Box<dyn Error>> {
        let argv: Vec<String> = std::env::args().skip(1).collect();
        let cli =
            Cli::parse(argv.clone().into_iter()).map_err(|error| format!("{error}\n\n{usage}"))?;
        if cli.help {
            println!("{usage}");
            return Ok(None);
        }
        let route = match mode {
            Mode::Live => Some(Route::from_env()?),
            Mode::Offline => None,
        };
        Ok(Some(Self {
            benchmark,
            started: Utc::now(),
            argv,
            cli,
            dir: dir.to_owned(),
            route,
            records: Vec::new(),
            tables: Vec::new(),
        }))
    }

    /// The route, for a live benchmark.
    #[must_use]
    pub fn route(&self) -> Option<&Route> {
        self.route.as_ref()
    }

    /// The parsed arguments.
    #[must_use]
    pub fn cli(&self) -> &Cli {
        &self.cli
    }

    /// Records one detail in the JSONL trail.
    pub fn record<T: Serialize>(&mut self, kind: &str, data: &T) {
        if let Ok(value) = serde_json::to_value(data) {
            self.records.push(JsonlRecord {
                kind: kind.to_owned(),
                at: Utc::now(),
                data: value,
            });
        }
    }

    /// Registers a named CSV table, written beside the report at the end.
    pub fn table(&mut self, name: &str, headers: &[&str], rows: Vec<Vec<String>>) {
        let mut csv = Csv::new(headers);
        for row in rows {
            csv.push(row);
        }
        self.tables.push((name.to_owned(), csv));
    }

    /// Writes the report, the trail and the manifest together.
    ///
    /// # Errors
    ///
    /// Returns the I/O error from the first artifact that cannot be written.
    pub fn finish(self, report: Report) -> Result<Artifacts, Box<dyn Error>> {
        let finished = Utc::now();
        let stem = self.stem();
        let report_path = self
            .cli
            .report
            .clone()
            .unwrap_or_else(|| default_path(&self.dir, &stem, "md"));
        write_text(&report_path, &report.finish())?;

        let jsonl_path = if self.records.is_empty() {
            None
        } else {
            let path = self
                .cli
                .jsonl
                .clone()
                .unwrap_or_else(|| default_path(&self.dir, &stem, "jsonl"));
            let mut jsonl = Jsonl::create(&path)?;
            for record in &self.records {
                jsonl.write(&record.kind, &record.data)?;
            }
            Some(path)
        };

        let manifest = Manifest {
            schema: "norte.bench.manifest/1".to_owned(),
            benchmark: self.benchmark.to_owned(),
            started: self.started,
            finished,
            argv: self.argv,
            commit: commit(),
            rustc: rustc_version(),
            route: self
                .route
                .as_ref()
                .and_then(|route| serde_json::to_value(route).ok()),
            env: redact_env(),
        };
        let manifest_path = default_path(&self.dir, &stem, "manifest.json");
        crate::artifacts::write_json(
            &manifest_path,
            &serde_json::to_value(&manifest).unwrap_or_else(|_| json!({})),
        )?;

        let mut csv = Vec::with_capacity(self.tables.len());
        for (name, table) in &self.tables {
            let path = default_path(&self.dir, &format!("{stem}-{name}"), "csv");
            write_csv(&path, table)?;
            csv.push(path);
        }

        let artifacts = Artifacts {
            report: report_path,
            jsonl: jsonl_path,
            csv,
            manifest: manifest_path,
        };
        artifacts.print();
        Ok(artifacts)
    }

    fn stem(&self) -> String {
        let at = self.started.format("%Y-%m-%dT%H%M%S");
        match &self.route {
            Some(route) => format!(
                "{at}-{}-{}",
                slug(route.provider.as_deref().unwrap_or("router")),
                slug(&route.model)
            ),
            None => format!("{at}-{}", slug(self.benchmark)),
        }
    }
}
