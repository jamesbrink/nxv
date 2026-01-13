//! Static analysis of all-packages.nix for file-to-attribute mapping.
//!
//! This module parses `all-packages.nix` using `rnix` to extract `callPackage`
//! patterns and build a reverse map from file paths to attribute names.
//!
//! This solves the fundamental limitation of `builtins.unsafeGetAttrPos` which
//! returns the assignment location (all-packages.nix) rather than the definition
//! location (e.g., pkgs/browsers/firefox/packages.nix).

use crate::error::Result;
use rnix::ast::{self, HasEntry};
use rowan::ast::AstNode;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use tracing::{debug, trace, warn};

/// The type of callPackage invocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CallKind {
    /// `callPackage ./path { }` - single package
    CallPackage,
    /// `callPackages ./path { }` - package set
    CallPackages,
}

/// A single callPackage hit extracted from the AST.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CallPackageHit {
    /// The attribute name (e.g., "firefox")
    pub attr_name: String,
    /// The path argument (e.g., "../applications/networking/browsers/firefox/packages.nix")
    pub path: String,
    /// Whether this is callPackage or callPackages
    pub kind: CallKind,
}

/// Result of static analysis of all-packages.nix.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StaticFileMap {
    /// file_path -> [attribute_names]
    /// Example: "pkgs/browsers/firefox/packages.nix" -> ["firefox", "firefox-esr"]
    pub file_to_attrs: HashMap<String, Vec<String>>,

    /// All extracted callPackage hits (for debugging)
    pub hits: Vec<CallPackageHit>,

    /// Attributes that couldn't be statically resolved
    pub unresolved_count: usize,

    /// Total attribute assignments found
    pub total_attrs: usize,
}

impl StaticFileMap {
    /// Calculate coverage ratio (resolved / total).
    pub fn coverage_ratio(&self) -> f64 {
        if self.total_attrs == 0 {
            return 0.0;
        }
        let resolved = self.hits.len();
        resolved as f64 / self.total_attrs as f64
    }

    /// Get attributes affected by a file change.
    pub fn attrs_for_file(&self, file_path: &str) -> Option<&Vec<String>> {
        self.file_to_attrs.get(file_path)
    }
}

/// Parse all-packages.nix content and extract file-to-attribute mappings.
///
/// # Arguments
/// * `content` - The content of all-packages.nix
/// * `base_path` - Base path for resolving relative paths (e.g., "pkgs/top-level")
///
/// # Returns
/// A `StaticFileMap` containing the extracted mappings.
pub fn parse_all_packages(content: &str, base_path: &str) -> Result<StaticFileMap> {
    let parse = rnix::Root::parse(content);

    // Check for parse errors but continue - rnix is resilient
    if !parse.errors().is_empty() {
        warn!(
            error_count = parse.errors().len(),
            "Parse errors in all-packages.nix (continuing with partial results)"
        );
        for err in parse.errors().iter().take(5) {
            trace!(error = ?err, "Parse error");
        }
    }

    let root = parse.tree();
    let mut result = StaticFileMap::default();
    let mut hits = Vec::new();

    // Find the root expression - all-packages.nix is typically:
    // { self, ... }: { attr1 = ...; attr2 = ...; }
    // We want to find the top-level attrset, not nested ones.
    if let Some(root_expr) = root.expr() {
        collect_from_expr(root_expr, "", &mut hits, &mut result.total_attrs);
    }

    // Build file-to-attrs map
    for hit in &hits {
        let normalized_path = normalize_path(&hit.path, base_path);
        result
            .file_to_attrs
            .entry(normalized_path)
            .or_default()
            .push(hit.attr_name.clone());
    }

    // Deduplicate attrs per file
    for attrs in result.file_to_attrs.values_mut() {
        attrs.sort();
        attrs.dedup();
    }

    result.unresolved_count = result.total_attrs.saturating_sub(hits.len());
    result.hits = hits;

    debug!(
        total_attrs = result.total_attrs,
        resolved = result.hits.len(),
        unresolved = result.unresolved_count,
        unique_files = result.file_to_attrs.len(),
        coverage = format!("{:.1}%", result.coverage_ratio() * 100.0),
        "Static analysis complete"
    );

    Ok(result)
}

/// Recursively collect entries from an expression, tracking the attribute path prefix.
///
/// This handles the structure of all-packages.nix where:
/// - Top-level is a function returning an attrset
/// - We want to track nested paths like `foo.bar = callPackage ...`
fn collect_from_expr(
    expr: ast::Expr,
    prefix: &str,
    hits: &mut Vec<CallPackageHit>,
    total_attrs: &mut usize,
) {
    let expr = strip_parens(expr);

    match expr {
        // Function: { args }: body - recurse into body
        ast::Expr::Lambda(lambda) => {
            if let Some(body) = lambda.body() {
                collect_from_expr(body, prefix, hits, total_attrs);
            }
        }
        // AttrSet: { attr1 = ...; attr2 = ...; }
        ast::Expr::AttrSet(attrset) => {
            collect_entries_with_prefix(attrset.entries(), prefix, hits, total_attrs);
        }
        // LetIn: let ... in body - process both bindings and body
        ast::Expr::LetIn(let_in) => {
            // Process let bindings (these define local variables, not exported attrs)
            // We still want to track them for coverage but they're usually not callPackage
            for entry in let_in.entries() {
                if let ast::Entry::AttrpathValue(_) = entry {
                    *total_attrs += 1;
                }
            }
            // Recurse into body
            if let Some(body) = let_in.body() {
                collect_from_expr(body, prefix, hits, total_attrs);
            }
        }
        // With: with expr; body - recurse into body
        ast::Expr::With(with_expr) => {
            if let Some(body) = with_expr.body() {
                collect_from_expr(body, prefix, hits, total_attrs);
            }
        }
        // Other expressions - don't recurse into nested attrsets
        _ => {}
    }
}

/// Collect entries from an iterator of Entry nodes with a prefix for nested attrs.
fn collect_entries_with_prefix<I>(
    entries: I,
    prefix: &str,
    hits: &mut Vec<CallPackageHit>,
    total_attrs: &mut usize,
) where
    I: Iterator<Item = ast::Entry>,
{
    for entry in entries {
        match entry {
            ast::Entry::AttrpathValue(apv) => {
                *total_attrs += 1;

                // Extract full attribute path (e.g., "foo.bar.baz")
                let attrpath = apv.attrpath().map(|p| {
                    p.attrs()
                        .filter_map(attr_text)
                        .collect::<Vec<_>>()
                        .join(".")
                });

                if let (Some(path_str), Some(value)) = (attrpath, apv.value()) {
                    // Combine prefix with attrpath
                    let full_name = if prefix.is_empty() {
                        path_str
                    } else {
                        format!("{}.{}", prefix, path_str)
                    };

                    if let Some((kind, path_expr)) = match_call(value) {
                        if let Some(path) = path_text(path_expr) {
                            hits.push(CallPackageHit {
                                attr_name: full_name,
                                path,
                                kind,
                            });
                        }
                    }
                }
            }
            ast::Entry::Inherit(inh) => {
                // Handle: inherit (callPackages ./path { }) foo bar;
                if let Some(from) = inh.from().and_then(|f| f.expr()) {
                    if let Some((kind, path_expr)) = match_call(from) {
                        if let Some(path) = path_text(path_expr) {
                            for attr in inh.attrs() {
                                *total_attrs += 1;
                                if let Some(name) = attr_text(attr) {
                                    let full_name = if prefix.is_empty() {
                                        name
                                    } else {
                                        format!("{}.{}", prefix, name)
                                    };
                                    hits.push(CallPackageHit {
                                        attr_name: full_name,
                                        path: path.clone(),
                                        kind,
                                    });
                                }
                            }
                        }
                    }
                } else {
                    // Plain inherit without from - count but can't resolve
                    for _ in inh.attrs() {
                        *total_attrs += 1;
                    }
                }
            }
        }
    }
}

/// Collect entries from an iterator of Entry nodes (no prefix, for tests).
#[cfg(test)]
fn collect_entries<I>(entries: I, hits: &mut Vec<CallPackageHit>, total_attrs: &mut usize)
where
    I: Iterator<Item = ast::Entry>,
{
    collect_entries_with_prefix(entries, "", hits, total_attrs);
}

/// Match a callPackage/callPackages expression.
///
/// Handles: `callPackage ./path { }` which parses as `Apply(Apply(callee, path), attrset)`
fn match_call(expr: ast::Expr) -> Option<(CallKind, ast::Expr)> {
    let expr = strip_parens(expr);

    // callPackage ./path { } is Apply(Apply(callee, path), attrset)
    let apply = ast::Apply::cast(expr.syntax().clone())?;
    let arg = apply.argument()?;
    let lambda = apply.lambda()?;

    // Inner Apply: Apply(callee, path)
    let inner = ast::Apply::cast(lambda.syntax().clone())?;
    let callee = inner.lambda()?;
    let path = inner.argument()?;

    // Check callee name
    let name = callee_name(callee)?;
    let kind = match name.as_str() {
        "callPackage" => CallKind::CallPackage,
        "callPackages" => CallKind::CallPackages,
        _ => return None,
    };

    // Verify final argument is an attrset (the override set)
    if ast::AttrSet::cast(arg.syntax().clone()).is_none() {
        return None;
    }

    Some((kind, path))
}

/// Strip parentheses from an expression.
fn strip_parens(expr: ast::Expr) -> ast::Expr {
    let mut cur = expr;
    loop {
        if let ast::Expr::Paren(ref p) = cur {
            if let Some(inner) = p.expr() {
                cur = inner;
                continue;
            }
        }
        return cur;
    }
}

/// Extract the function name from a callee expression.
///
/// Handles:
/// - `callPackage` -> "callPackage"
/// - `pkgs.callPackage` -> "callPackage" (Select expression)
fn callee_name(expr: ast::Expr) -> Option<String> {
    let expr = strip_parens(expr);
    match expr {
        ast::Expr::Ident(id) => ident_text(id),
        ast::Expr::Select(sel) => {
            // pkgs.callPackage -> take last attr
            let attr = sel.attrpath()?.attrs().last()?;
            attr_text(attr)
        }
        _ => None,
    }
}

/// Extract text from an Ident node.
fn ident_text(ident: ast::Ident) -> Option<String> {
    ident.ident_token().map(|t| t.text().to_string())
}

/// Extract text from an Attr node (attribute name).
fn attr_text(attr: ast::Attr) -> Option<String> {
    match attr {
        ast::Attr::Ident(id) => ident_text(id),
        ast::Attr::Str(s) => {
            // String attribute names - strip quotes
            let text = s.syntax().text().to_string();
            Some(text.trim_matches('"').to_string())
        }
        ast::Attr::Dynamic(_) => None, // Can't resolve ${expr} statically
    }
}

/// Extract path text from a path expression.
fn path_text(expr: ast::Expr) -> Option<String> {
    let expr = strip_parens(expr);
    match expr {
        ast::Expr::Path(p) => Some(p.syntax().text().to_string()),
        ast::Expr::Str(s) => {
            let text = s.syntax().text().to_string();
            Some(text.trim_matches('"').to_string())
        }
        _ => None, // Can't resolve complex expressions
    }
}

/// Normalize a relative path to a canonical form.
///
/// Converts paths like "../applications/firefox" relative to "pkgs/top-level"
/// into "pkgs/applications/firefox".
fn normalize_path(path: &str, base_path: &str) -> String {
    // Handle absolute paths (rare but possible)
    if path.starts_with('/') {
        return path.to_string();
    }

    // Split base path into components
    let mut components: Vec<&str> = base_path.split('/').filter(|s| !s.is_empty()).collect();

    // Process each component of the relative path
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                components.pop();
            }
            other => {
                components.push(other);
            }
        }
    }

    components.join("/")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Test against real all-packages.nix if available.
    /// Run with: cargo test --features indexer test_real_all_packages -- --ignored --nocapture
    #[test]
    #[ignore]
    fn test_real_all_packages() {
        let nixpkgs_path = std::env::var("NIXPKGS_PATH")
            .unwrap_or_else(|_| "nixpkgs".to_string());
        let all_packages_path = format!("{}/pkgs/top-level/all-packages.nix", nixpkgs_path);

        let content = match std::fs::read_to_string(&all_packages_path) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("Skipping test: {} not found: {}", all_packages_path, e);
                return;
            }
        };

        eprintln!("Parsing {} ({} bytes)...", all_packages_path, content.len());

        let start = std::time::Instant::now();
        let result = parse_all_packages(&content, "pkgs/top-level").unwrap();
        let elapsed = start.elapsed();

        eprintln!("\n=== Static Analysis Results ===");
        eprintln!("Parse time: {:?}", elapsed);
        eprintln!("Total attributes: {}", result.total_attrs);
        eprintln!("Resolved (callPackage): {}", result.hits.len());
        eprintln!("Unresolved: {}", result.unresolved_count);
        eprintln!("Coverage: {:.1}%", result.coverage_ratio() * 100.0);
        eprintln!("Unique files: {}", result.file_to_attrs.len());

        // Sample of resolved hits
        eprintln!("\n=== Sample Resolved Hits (first 20) ===");
        for hit in result.hits.iter().take(20) {
            eprintln!("  {} -> {} ({:?})", hit.attr_name, hit.path, hit.kind);
        }

        // Sample of file mappings
        eprintln!("\n=== Sample File Mappings (first 10) ===");
        for (file, attrs) in result.file_to_attrs.iter().take(10) {
            eprintln!("  {} -> {:?}", file, attrs);
        }

        // Assert reasonable coverage (we expect 30-50% for all-packages.nix)
        // because many attrs use inherit, let bindings, etc.
        assert!(result.coverage_ratio() > 0.2, "Coverage too low: {:.1}%", result.coverage_ratio() * 100.0);
        assert!(result.hits.len() > 1000, "Expected >1000 callPackage hits, got {}", result.hits.len());
    }

    #[test]
    fn test_simple_callpackage() {
        let content = r#"
        {
            firefox = callPackage ./browsers/firefox { };
        }
        "#;

        let result = parse_all_packages(content, "pkgs").unwrap();
        assert_eq!(result.hits.len(), 1);
        assert_eq!(result.hits[0].attr_name, "firefox");
        assert_eq!(result.hits[0].path, "./browsers/firefox");
        assert_eq!(result.hits[0].kind, CallKind::CallPackage);
    }

    #[test]
    fn test_callpackage_with_args() {
        let content = r#"
        {
            vim = callPackage ./editors/vim { gui = false; };
        }
        "#;

        let result = parse_all_packages(content, "pkgs").unwrap();
        assert_eq!(result.hits.len(), 1);
        assert_eq!(result.hits[0].attr_name, "vim");
    }

    #[test]
    fn test_callpackages_plural() {
        let content = r#"
        {
            firefoxPackages = callPackages ./browsers/firefox/packages.nix { };
        }
        "#;

        let result = parse_all_packages(content, "pkgs").unwrap();
        assert_eq!(result.hits.len(), 1);
        assert_eq!(result.hits[0].kind, CallKind::CallPackages);
    }

    #[test]
    fn test_inherit_from_callpackages() {
        let content = r#"
        {
            inherit (callPackages ./browsers/firefox { }) firefox firefox-esr;
        }
        "#;

        let result = parse_all_packages(content, "pkgs").unwrap();
        assert_eq!(result.hits.len(), 2);

        let names: Vec<_> = result.hits.iter().map(|h| &h.attr_name).collect();
        assert!(names.contains(&&"firefox".to_string()));
        assert!(names.contains(&&"firefox-esr".to_string()));
    }

    #[test]
    fn test_pkgs_callpackage() {
        let content = r#"
        {
            hello = pkgs.callPackage ./misc/hello { };
        }
        "#;

        let result = parse_all_packages(content, "pkgs").unwrap();
        assert_eq!(result.hits.len(), 1);
        assert_eq!(result.hits[0].attr_name, "hello");
    }

    #[test]
    fn test_normalize_path() {
        assert_eq!(
            normalize_path("../applications/firefox", "pkgs/top-level"),
            "pkgs/applications/firefox"
        );
        assert_eq!(
            normalize_path("./browsers/firefox", "pkgs/top-level"),
            "pkgs/top-level/browsers/firefox"
        );
        assert_eq!(
            normalize_path("../../by-name/fi/firefox", "pkgs/top-level"),
            "by-name/fi/firefox"
        );
    }

    #[test]
    fn test_coverage_calculation() {
        let content = r#"
        {
            resolved = callPackage ./path { };
            unresolved = someOtherFunction ./path;
            alsoUnresolved = import ./path;
        }
        "#;

        let result = parse_all_packages(content, "pkgs").unwrap();
        assert_eq!(result.total_attrs, 3);
        assert_eq!(result.hits.len(), 1);
        assert_eq!(result.unresolved_count, 2);
        assert!((result.coverage_ratio() - 0.333).abs() < 0.01);
    }

    #[test]
    fn test_file_to_attrs_map() {
        let content = r#"
        {
            firefox = callPackage ./browsers/firefox { };
            firefoxDev = callPackage ./browsers/firefox { dev = true; };
        }
        "#;

        let result = parse_all_packages(content, "pkgs").unwrap();
        let attrs = result.attrs_for_file("pkgs/browsers/firefox").unwrap();
        assert_eq!(attrs.len(), 2);
        assert!(attrs.contains(&"firefox".to_string()));
        assert!(attrs.contains(&"firefoxDev".to_string()));
    }

    #[test]
    fn test_full_attrpath() {
        // Test that we extract full attrpaths like foo.bar, not just foo
        let content = r#"
        {
            python3Packages.requests = callPackage ./python-modules/requests { };
            qt6.qtbase = callPackage ./qt/qtbase { };
        }
        "#;

        let result = parse_all_packages(content, "pkgs").unwrap();
        assert_eq!(result.hits.len(), 2);

        let names: Vec<_> = result.hits.iter().map(|h| h.attr_name.as_str()).collect();
        assert!(names.contains(&"python3Packages.requests"), "Expected python3Packages.requests, got {:?}", names);
        assert!(names.contains(&"qt6.qtbase"), "Expected qt6.qtbase, got {:?}", names);
    }

    #[test]
    fn test_lambda_body() {
        // Test that we handle function definitions correctly
        let content = r#"
        { lib, pkgs }:
        {
            hello = callPackage ./hello { };
        }
        "#;

        let result = parse_all_packages(content, "pkgs").unwrap();
        assert_eq!(result.hits.len(), 1);
        assert_eq!(result.hits[0].attr_name, "hello");
    }

    #[test]
    fn test_let_in_body() {
        // Test that let bindings don't produce false positives
        let content = r#"
        let
            helper = ./helper.nix;
        in
        {
            hello = callPackage ./hello { };
        }
        "#;

        let result = parse_all_packages(content, "pkgs").unwrap();
        assert_eq!(result.hits.len(), 1);
        assert_eq!(result.hits[0].attr_name, "hello");
        // The let binding should be counted but not as a callPackage hit
        assert_eq!(result.total_attrs, 2); // let binding + attrset entry
    }
}
