//! A tabular artifact, for the reader who lives in a spreadsheet.

use std::error::Error;
use std::path::Path;

/// A table rendered as CSV, escaped per RFC 4180.
#[derive(Debug, Clone)]
pub struct Csv {
    headers: Vec<String>,
    rows: Vec<Vec<String>>,
}

impl Csv {
    /// Starts a table with a header row.
    #[must_use]
    pub fn new(headers: &[&str]) -> Self {
        Self {
            headers: headers.iter().map(|header| (*header).to_owned()).collect(),
            rows: Vec::new(),
        }
    }

    /// Appends one row. A short row is padded, a long one is kept whole.
    pub fn push(&mut self, row: Vec<String>) {
        self.rows.push(row);
    }

    /// How many data rows the table holds.
    #[must_use]
    pub fn len(&self) -> usize {
        self.rows.len()
    }

    /// Whether the table has no data rows.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// Renders the table, header included.
    #[must_use]
    pub fn render(&self) -> String {
        let mut out = String::new();
        let width = self.headers.len().max(
            self.rows
                .iter()
                .map(Vec::len)
                .max()
                .unwrap_or(self.headers.len()),
        );
        write_row(&mut out, &self.headers, width);
        for row in &self.rows {
            write_row(&mut out, row, width);
        }
        out
    }
}

fn write_row(out: &mut String, cells: &[String], width: usize) {
    for index in 0..width {
        if index > 0 {
            out.push(',');
        }
        let cell = cells.get(index).map_or("", String::as_str);
        out.push_str(&escape(cell));
    }
    out.push('\n');
}

/// Escapes one field: quotes it when it carries a comma, a quote or a newline.
#[must_use]
pub fn escape(field: &str) -> String {
    if field.contains(',') || field.contains('"') || field.contains('\n') || field.contains('\r') {
        format!("\"{}\"", field.replace('"', "\"\""))
    } else {
        field.to_owned()
    }
}

/// Writes a table, making the directory if it has to.
///
/// # Errors
///
/// Returns the I/O error when the directory or the file cannot be written.
pub fn write_csv(path: &Path, csv: &Csv) -> Result<(), Box<dyn Error>> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, csv.render())?;
    Ok(())
}
