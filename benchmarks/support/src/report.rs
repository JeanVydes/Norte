//! A markdown report builder: sections, tables and simple charts.

use std::fmt::Write as _;

/// A column alignment for [`Report::table`].
#[derive(Debug, Clone, Copy)]
pub enum Align {
    /// Text starts at the left edge.
    Left,
    /// Numbers line up at the right edge.
    Right,
}

/// A markdown document under construction.
pub struct Report {
    out: String,
}

impl Report {
    /// Starts a document with a title.
    #[must_use]
    pub fn new(title: &str) -> Self {
        Self {
            out: format!("# {title}\n"),
        }
    }

    /// Adds a level-two section.
    pub fn section(&mut self, title: &str) -> &mut Self {
        let _ = writeln!(self.out, "\n## {title}\n");
        self
    }

    /// Adds a paragraph.
    pub fn paragraph(&mut self, text: &str) -> &mut Self {
        let _ = writeln!(self.out, "{text}\n");
        self
    }

    /// Adds a two-column key/value table.
    pub fn kv(&mut self, rows: &[(&str, String)]) -> &mut Self {
        let rows: Vec<Vec<String>> = rows
            .iter()
            .map(|(key, value)| vec![(*key).to_owned(), value.clone()])
            .collect();
        self.table(&["", ""], &[Align::Left, Align::Left], &rows)
    }

    /// Adds a table with a header row.
    pub fn table(&mut self, headers: &[&str], aligns: &[Align], rows: &[Vec<String>]) -> &mut Self {
        self.out.push('|');
        for header in headers {
            let _ = write!(self.out, " {header} |");
        }
        self.out.push('\n');
        self.out.push('|');
        for align in aligns {
            self.out.push_str(match align {
                Align::Left => " --- |",
                Align::Right => " ---: |",
            });
        }
        self.out.push('\n');
        for row in rows {
            self.out.push('|');
            for cell in row {
                let _ = write!(self.out, " {cell} |");
            }
            self.out.push('\n');
        }
        self.out.push('\n');
        self
    }

    /// Adds a horizontal bar chart, scaled to the largest value.
    pub fn bars(&mut self, rows: &[(String, u64)], width: usize) -> &mut Self {
        let max = rows
            .iter()
            .map(|(_, value)| *value)
            .max()
            .unwrap_or(1)
            .max(1);
        let width_u64 = u64::try_from(width).unwrap_or(1);
        for (label, value) in rows {
            let filled = usize::try_from(value.saturating_mul(width_u64) / max).unwrap_or(width);
            let filled = filled.min(width);
            let bar = "█".repeat(filled);
            let rest = "░".repeat(width - filled);
            let _ = writeln!(self.out, "{label}  {bar}{rest}  {value}");
        }
        self.out.push('\n');
        self
    }

    /// Adds a labelled sparkline.
    pub fn sparkline(&mut self, label: &str, values: &[u64]) -> &mut Self {
        let _ = writeln!(self.out, "{label}  `{}`", crate::time::sparkline(values));
        self
    }

    /// Adds a fenced code block.
    pub fn code(&mut self, language: &str, body: &str) -> &mut Self {
        let _ = writeln!(self.out, "```{language}\n{body}\n```\n");
        self
    }

    /// Appends raw markdown.
    pub fn lines(&mut self, text: &str) -> &mut Self {
        self.out.push_str(text);
        self.out.push('\n');
        self
    }

    /// The finished document.
    #[must_use]
    pub fn finish(self) -> String {
        self.out
    }
}
