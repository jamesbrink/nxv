//! Output formatting for search results.

pub mod json;
pub mod plain;
pub mod table;

use crate::db::queries::PackageVersion;

/// Output format options.
#[derive(Debug, Clone, Copy, Default)]
pub enum OutputFormat {
    /// Colored table output (default).
    #[default]
    Table,
    /// JSON output.
    Json,
    /// Plain text output (no colors).
    Plain,
}

/// Table display options.
#[derive(Debug, Clone, Copy, Default)]
pub struct TableOptions {
    /// Show platforms column.
    pub show_platforms: bool,
    /// Use ASCII borders instead of Unicode.
    pub ascii: bool,
}

/// Format and print search results.
pub fn print_results(results: &[PackageVersion], format: OutputFormat, options: TableOptions) {
    match format {
        OutputFormat::Table => table::print_table(results, options),
        OutputFormat::Json => json::print_json(results),
        OutputFormat::Plain => plain::print_plain(results, options.show_platforms),
    }
}
