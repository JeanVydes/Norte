//! Argument parsing shared by the benchmarks.

use std::collections::BTreeMap;
use std::path::PathBuf;

/// The arguments every benchmark understands.
#[derive(Debug, Default)]
pub struct Cli {
    /// Where to write the markdown report.
    pub report: Option<PathBuf>,
    /// Where to write the JSONL trail.
    pub jsonl: Option<PathBuf>,
    /// Whether `--help` was asked for.
    pub help: bool,
    /// Free `--name value` options; a flag without a value becomes `true`.
    pub options: BTreeMap<String, String>,
}

impl Cli {
    /// Parses the arguments after the program name.
    ///
    /// # Errors
    ///
    /// Returns a message naming the argument when a value is missing or an
    /// argument is not understood.
    pub fn parse<I: Iterator<Item = String>>(args: I) -> Result<Self, String> {
        let mut cli = Self::default();
        let mut args = args.peekable();
        while let Some(argument) = args.next() {
            match argument.as_str() {
                "--report" => {
                    let path = args.next().ok_or("--report needs a path")?;
                    cli.report = Some(PathBuf::from(path));
                }
                "--jsonl" => {
                    let path = args.next().ok_or("--jsonl needs a path")?;
                    cli.jsonl = Some(PathBuf::from(path));
                }
                "--help" | "-h" => cli.help = true,
                other => {
                    let name = other.trim_start_matches('-');
                    if name.is_empty() || !other.starts_with("--") {
                        return Err(format!("unknown argument: {other}"));
                    }
                    let value = if matches!(args.peek(), Some(next) if !next.starts_with("--")) {
                        args.next().unwrap_or_else(|| "true".to_owned())
                    } else {
                        "true".to_owned()
                    };
                    cli.options.insert(name.to_owned(), value);
                }
            }
        }
        Ok(cli)
    }

    /// A free option's value.
    #[must_use]
    pub fn option(&self, name: &str) -> Option<&str> {
        self.options.get(name).map(String::as_str)
    }

    /// A free option parsed as a whole number.
    ///
    /// # Errors
    ///
    /// Returns a message naming the option when its value is not a whole number.
    pub fn option_u64(&self, name: &str) -> Result<Option<u64>, String> {
        match self.options.get(name) {
            None => Ok(None),
            Some(value) => value
                .parse::<u64>()
                .map(Some)
                .map_err(|_| format!("--{name} needs a whole number, got `{value}`")),
        }
    }

    /// A free option that has to be present.
    ///
    /// # Errors
    ///
    /// Returns a message naming the option when it is absent.
    pub fn required_option(&self, name: &str) -> Result<&str, String> {
        self.options
            .get(name)
            .map(String::as_str)
            .ok_or_else(|| format!("--{name} is required"))
    }
}
