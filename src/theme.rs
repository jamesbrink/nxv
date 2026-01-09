//! Centralized color theming for consistent CLI output.
//!
//! This module provides a semantic color palette that works with both
//! `owo_colors` (for terminal text) and `comfy_table` (for tables).
//!
//! # NO_COLOR Support
//!
//! Colors can be disabled globally via:
//! - The `--no-color` CLI flag
//! - The `NO_COLOR` environment variable
//!
//! When colors are disabled, all theming functions return unstyled output.
//!
//! # Usage
//!
//! For terminal output with owo_colors:
//! ```ignore
//! use crate::theme::Themed;
//! println!("{}", pkg.name.attr_path());
//! println!("{}", "Success!".success());
//! ```
//!
//! For comfy_table cells:
//! ```ignore
//! use crate::theme::{Semantic, ThemedCell};
//! Cell::new(&pkg.name).themed(Semantic::AttrPath)
//! ```

use std::sync::atomic::{AtomicBool, Ordering};

/// Global color enable flag (respects NO_COLOR and --no-color).
static COLORS_ENABLED: AtomicBool = AtomicBool::new(true);

/// Disable all colors globally.
///
/// This affects both owo_colors output and comfy_table cells.
/// Call this early in main() when --no-color is set.
pub fn disable_colors() {
    COLORS_ENABLED.store(false, Ordering::Relaxed);
    owo_colors::set_override(false);
}

/// Check if colors are currently enabled.
pub fn colors_enabled() -> bool {
    COLORS_ENABLED.load(Ordering::Relaxed)
}

/// Semantic color categories for consistent theming.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Semantic {
    /// Package attribute paths (e.g., "python311", "nodePackages.typescript")
    AttrPath,
    /// Package versions (safe)
    Version,
    /// Package versions with known vulnerabilities
    VersionInsecure,
    /// Git commit hashes
    Commit,
    /// Dates and timestamps
    Date,
    /// Nix store paths
    StorePath,
    /// Muted/secondary text
    Muted,
    /// Description text
    Description,
}

/// Get the comfy_table color for a semantic category.
///
/// Returns `None` when colors are disabled, which leaves the cell unstyled.
pub fn table_color(semantic: Semantic) -> Option<comfy_table::Color> {
    if !colors_enabled() {
        return None;
    }
    Some(match semantic {
        Semantic::AttrPath => comfy_table::Color::Cyan,
        Semantic::Version => comfy_table::Color::Green,
        Semantic::VersionInsecure => comfy_table::Color::Red,
        Semantic::Commit => comfy_table::Color::Yellow,
        Semantic::Date => comfy_table::Color::Reset,
        Semantic::StorePath => comfy_table::Color::Magenta,
        Semantic::Muted => comfy_table::Color::Reset,
        Semantic::Description => comfy_table::Color::Reset,
    })
}

/// Extension trait for comfy_table cells with NO_COLOR support.
pub trait ThemedCell {
    /// Apply semantic coloring to a cell, respecting NO_COLOR.
    fn themed(self, semantic: Semantic) -> Self;
}

impl ThemedCell for comfy_table::Cell {
    fn themed(self, semantic: Semantic) -> Self {
        match table_color(semantic) {
            Some(color) => self.fg(color),
            None => self,
        }
    }
}

/// Extension trait for applying semantic colors with owo_colors.
///
/// This trait provides semantic color methods that make code more readable
/// and ensure consistency across the codebase.
///
/// All methods respect the global color enable state set by `disable_colors()`.
#[allow(dead_code)] // Not all methods are used by all features
pub trait Themed: owo_colors::OwoColorize {
    /// Style for package attribute paths (cyan).
    fn attr_path(&self) -> String
    where
        Self: std::fmt::Display,
    {
        if colors_enabled() {
            format!("{}", owo_colors::OwoColorize::cyan(self))
        } else {
            self.to_string()
        }
    }

    /// Style for package versions (green).
    fn version(&self) -> String
    where
        Self: std::fmt::Display,
    {
        if colors_enabled() {
            format!("{}", owo_colors::OwoColorize::green(self))
        } else {
            self.to_string()
        }
    }

    /// Style for insecure package versions (red).
    fn version_insecure(&self) -> String
    where
        Self: std::fmt::Display,
    {
        if colors_enabled() {
            format!("{}", owo_colors::OwoColorize::red(self))
        } else {
            self.to_string()
        }
    }

    /// Style for git commit hashes (yellow).
    fn commit(&self) -> String
    where
        Self: std::fmt::Display,
    {
        if colors_enabled() {
            format!("{}", owo_colors::OwoColorize::yellow(self))
        } else {
            self.to_string()
        }
    }

    /// Style for store paths (magenta).
    fn store_path(&self) -> String
    where
        Self: std::fmt::Display,
    {
        if colors_enabled() {
            format!("{}", owo_colors::OwoColorize::magenta(self))
        } else {
            self.to_string()
        }
    }

    /// Style for error messages (red + bold).
    fn error_style(&self) -> String
    where
        Self: std::fmt::Display,
    {
        if colors_enabled() {
            format!(
                "{}",
                owo_colors::OwoColorize::bold(&owo_colors::OwoColorize::red(self))
            )
        } else {
            self.to_string()
        }
    }

    /// Style for warning messages (yellow).
    fn warning(&self) -> String
    where
        Self: std::fmt::Display,
    {
        if colors_enabled() {
            format!("{}", owo_colors::OwoColorize::yellow(self))
        } else {
            self.to_string()
        }
    }

    /// Style for success messages (green + bold).
    fn success(&self) -> String
    where
        Self: std::fmt::Display,
    {
        if colors_enabled() {
            format!(
                "{}",
                owo_colors::OwoColorize::bold(&owo_colors::OwoColorize::green(self))
            )
        } else {
            self.to_string()
        }
    }

    /// Style for section headers (bold + underline).
    fn section_header(&self) -> String
    where
        Self: std::fmt::Display,
    {
        if colors_enabled() {
            format!(
                "{}",
                owo_colors::OwoColorize::underline(&owo_colors::OwoColorize::bold(self))
            )
        } else {
            self.to_string()
        }
    }

    /// Style for section headers with error emphasis (bold + underline + red).
    fn section_header_error(&self) -> String
    where
        Self: std::fmt::Display,
    {
        if colors_enabled() {
            format!(
                "{}",
                owo_colors::OwoColorize::red(&owo_colors::OwoColorize::underline(
                    &owo_colors::OwoColorize::bold(self)
                ))
            )
        } else {
            self.to_string()
        }
    }

    /// Style for labels/field names (yellow).
    fn label(&self) -> String
    where
        Self: std::fmt::Display,
    {
        if colors_enabled() {
            format!("{}", owo_colors::OwoColorize::yellow(self))
        } else {
            self.to_string()
        }
    }

    /// Style for bold text.
    fn bold_text(&self) -> String
    where
        Self: std::fmt::Display,
    {
        if colors_enabled() {
            format!("{}", owo_colors::OwoColorize::bold(self))
        } else {
            self.to_string()
        }
    }

    /// Style for count/numeric values in progress output (cyan).
    fn count(&self) -> String
    where
        Self: std::fmt::Display,
    {
        if colors_enabled() {
            format!("{}", owo_colors::OwoColorize::cyan(self))
        } else {
            self.to_string()
        }
    }

    /// Style for "found" counts in progress output (green).
    fn count_found(&self) -> String
    where
        Self: std::fmt::Display,
    {
        if colors_enabled() {
            format!("{}", owo_colors::OwoColorize::green(self))
        } else {
            self.to_string()
        }
    }

    /// Style for "pending" counts in progress output (yellow).
    fn count_pending(&self) -> String
    where
        Self: std::fmt::Display,
    {
        if colors_enabled() {
            format!("{}", owo_colors::OwoColorize::yellow(self))
        } else {
            self.to_string()
        }
    }

    /// Style for highlighted/current platform (green + bold).
    fn highlight(&self) -> String
    where
        Self: std::fmt::Display,
    {
        if colors_enabled() {
            format!(
                "{}",
                owo_colors::OwoColorize::bold(&owo_colors::OwoColorize::green(self))
            )
        } else {
            self.to_string()
        }
    }
}

// Implement Themed for common types
impl Themed for String {}
impl Themed for &str {}
impl Themed for i32 {}
impl Themed for i64 {}
impl Themed for u32 {}
impl Themed for u64 {}
impl Themed for usize {}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;

    // Reset color state before each test
    fn reset_colors() {
        COLORS_ENABLED.store(true, Ordering::Relaxed);
        owo_colors::set_override(true);
    }

    #[test]
    #[serial(colors)]
    fn test_colors_enabled_by_default() {
        reset_colors();
        assert!(colors_enabled());
    }

    #[test]
    #[serial(colors)]
    fn test_disable_colors() {
        reset_colors();
        assert!(colors_enabled());
        disable_colors();
        assert!(!colors_enabled());
        // Reset for other tests
        reset_colors();
    }

    #[test]
    #[serial(colors)]
    fn test_themed_trait_with_colors() {
        reset_colors();
        let text = "test";

        // When colors are enabled, output should contain ANSI codes
        let colored = text.attr_path();
        assert!(colored.contains("\x1b["), "Expected ANSI escape codes");
        assert!(colored.contains("test"));
    }

    #[test]
    #[serial(colors)]
    fn test_themed_trait_without_colors() {
        reset_colors();
        disable_colors();

        let text = "test";

        // When colors are disabled, output should be plain
        assert_eq!(text.attr_path(), "test");
        assert_eq!(text.version(), "test");
        assert_eq!(text.commit(), "test");
        assert_eq!(text.error_style(), "test");
        assert_eq!(text.warning(), "test");
        assert_eq!(text.success(), "test");
        assert_eq!(text.section_header(), "test");
        assert_eq!(text.label(), "test");

        // Reset for other tests
        reset_colors();
    }

    #[test]
    #[serial(colors)]
    fn test_table_color_with_colors() {
        reset_colors();

        assert_eq!(
            table_color(Semantic::AttrPath),
            Some(comfy_table::Color::Cyan)
        );
        assert_eq!(
            table_color(Semantic::Version),
            Some(comfy_table::Color::Green)
        );
        assert_eq!(
            table_color(Semantic::VersionInsecure),
            Some(comfy_table::Color::Red)
        );
        assert_eq!(
            table_color(Semantic::Commit),
            Some(comfy_table::Color::Yellow)
        );
        assert_eq!(
            table_color(Semantic::StorePath),
            Some(comfy_table::Color::Magenta)
        );
    }

    #[test]
    #[serial(colors)]
    fn test_table_color_without_colors() {
        reset_colors();
        disable_colors();

        assert_eq!(table_color(Semantic::AttrPath), None);
        assert_eq!(table_color(Semantic::Version), None);
        assert_eq!(table_color(Semantic::Commit), None);

        // Reset for other tests
        reset_colors();
    }

    #[test]
    #[serial(colors)]
    fn test_themed_cell() {
        reset_colors();

        let cell = comfy_table::Cell::new("test").themed(Semantic::AttrPath);
        // Cell should be created without panicking
        assert!(!cell.content().is_empty());
    }

    #[test]
    #[serial(colors)]
    fn test_themed_cell_no_color() {
        reset_colors();
        disable_colors();

        let cell = comfy_table::Cell::new("test").themed(Semantic::AttrPath);
        // Cell should still work when colors are disabled
        assert!(!cell.content().is_empty());

        // Reset for other tests
        reset_colors();
    }

    #[test]
    #[serial(colors)]
    fn test_numeric_types() {
        reset_colors();

        // Test that numeric types work with the theme
        let num: usize = 42;
        let result = num.count();
        assert!(result.contains("42"));

        let num_i32: i32 = -10;
        let result = num_i32.count();
        assert!(result.contains("-10"));
    }

    #[test]
    #[serial(colors)]
    fn test_all_semantic_variants() {
        reset_colors();

        // Ensure all semantic variants have defined colors
        let variants = [
            Semantic::AttrPath,
            Semantic::Version,
            Semantic::VersionInsecure,
            Semantic::Commit,
            Semantic::Date,
            Semantic::StorePath,
            Semantic::Muted,
            Semantic::Description,
        ];

        for variant in variants {
            // Should not panic and should return Some when colors enabled
            let color = table_color(variant);
            assert!(color.is_some(), "Expected color for {:?}", variant);
        }
    }

    #[test]
    fn test_semantic_debug() {
        // Ensure Debug is implemented
        let s = Semantic::AttrPath;
        let debug = format!("{:?}", s);
        assert!(debug.contains("AttrPath"));
    }

    #[test]
    fn test_semantic_equality() {
        assert_eq!(Semantic::AttrPath, Semantic::AttrPath);
        assert_ne!(Semantic::AttrPath, Semantic::Version);
    }
}
