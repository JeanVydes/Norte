//! The files a run leaves behind: a JSONL trail and a run manifest.

use std::collections::BTreeMap;
use std::error::Error;
use std::fs::File;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::Value;

/// One line of the JSONL trail: a kind, a moment and the record.
#[derive(Debug, Clone, Serialize)]
pub struct JsonlRecord {
    /// What the record is: `request`, `attempt`, `case`, `turn`…
    pub kind: String,
    /// When it happened.
    pub at: DateTime<Utc>,
    /// The record itself, as the benchmark measured it.
    pub data: Value,
}

/// An append-only JSONL file.
pub struct Jsonl {
    file: File,
    path: PathBuf,
}

impl Jsonl {
    /// Creates (or truncates) a JSONL file, making the directory if it has to.
    ///
    /// # Errors
    ///
    /// Returns the I/O error when the directory or the file cannot be created.
    pub fn create(path: &Path) -> Result<Self, Box<dyn Error>> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        Ok(Self {
            file: File::create(path)?,
            path: path.to_owned(),
        })
    }

    /// Appends one record.
    ///
    /// # Errors
    ///
    /// Returns the I/O error when the line cannot be written.
    pub fn write<T: Serialize>(&mut self, kind: &str, data: &T) -> Result<(), Box<dyn Error>> {
        let record = JsonlRecord {
            kind: kind.to_owned(),
            at: Utc::now(),
            data: serde_json::to_value(data)?,
        };
        writeln!(self.file, "{}", serde_json::to_string(&record)?)?;
        Ok(())
    }

    /// The path this trail was created at.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// What a run is, pinned so a number can be reproduced.
#[derive(Debug, Clone, Serialize)]
pub struct Manifest {
    /// The artifact schema, so a reader knows what it is reading.
    pub schema: String,
    /// The benchmark that wrote it.
    pub benchmark: String,
    /// When the run started.
    pub started: DateTime<Utc>,
    /// When the run finished.
    pub finished: DateTime<Utc>,
    /// The arguments the run was given.
    pub argv: Vec<String>,
    /// The commit the run started from, when it ran from a checkout.
    pub commit: Option<String>,
    /// The compiler that built the benchmark.
    pub rustc: Option<String>,
    /// The route, for a live benchmark.
    pub route: Option<Value>,
    /// The environment, with anything key-shaped redacted.
    pub env: BTreeMap<String, String>,
}

/// The files a run wrote.
#[derive(Debug, Clone)]
pub struct Artifacts {
    /// The markdown report.
    pub report: PathBuf,
    /// The JSONL trail, when the run recorded anything.
    pub jsonl: Option<PathBuf>,
    /// The CSV tables, one path per named table.
    pub csv: Vec<PathBuf>,
    /// The run manifest.
    pub manifest: PathBuf,
}

impl Artifacts {
    /// Prints the paths of everything the run wrote, one per line.
    pub fn print(&self) {
        println!("\nreport:   {}", self.report.display());
        if let Some(jsonl) = &self.jsonl {
            println!("jsonl:    {}", jsonl.display());
        }
        for path in &self.csv {
            println!("csv:      {}", path.display());
        }
        println!("manifest: {}", self.manifest.display());
    }
}

/// Where a run writes when told nowhere else: beside its crate, under
/// `results/`.
#[must_use]
pub fn default_path(dir: &Path, stem: &str, extension: &str) -> PathBuf {
    dir.join("results").join(format!("{stem}.{extension}"))
}

/// A file-name-safe form of a name: `deepseek/deepseek-v4-flash-0731` becomes
/// `deepseek-deepseek-v4-flash-0731`.
#[must_use]
pub fn slug(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for character in text.chars() {
        if character.is_ascii_alphanumeric() {
            out.push(character.to_ascii_lowercase());
        } else if !out.is_empty() && !out.ends_with('-') {
            out.push('-');
        }
    }
    out.trim_end_matches('-').to_owned()
}

/// The commit the run started from, when it is run from a checkout. Optional on
/// purpose: a benchmark has to run from a tarball too.
#[must_use]
pub fn commit() -> Option<String> {
    let output = std::process::Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let sha = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    (!sha.is_empty()).then_some(sha)
}

/// The compiler that built the run, for traceability.
#[must_use]
pub fn rustc_version() -> Option<String> {
    let output = std::process::Command::new("rustc")
        .arg("--version")
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

/// The environment, with values whose names look like secrets replaced by
/// `***`, so a report can be shared without leaking a key.
#[must_use]
pub fn redact_env() -> BTreeMap<String, String> {
    let mut env = BTreeMap::new();
    for (name, value) in std::env::vars() {
        let upper = name.to_ascii_uppercase();
        let secret = upper.contains("KEY")
            || upper.contains("TOKEN")
            || upper.contains("SECRET")
            || upper.contains("PASSWORD");
        env.insert(name, if secret { "***".to_owned() } else { value });
    }
    env
}

/// Writes a value as pretty JSON, making the directory if it has to.
///
/// # Errors
///
/// Returns the serialization or I/O error.
pub fn write_json(path: &Path, value: &Value) -> Result<(), Box<dyn Error>> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, serde_json::to_string_pretty(value)?)?;
    Ok(())
}

/// Writes text, making the directory if it has to.
///
/// # Errors
///
/// Returns the I/O error.
pub fn write_text(path: &Path, text: &str) -> Result<(), Box<dyn Error>> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, text)?;
    Ok(())
}
