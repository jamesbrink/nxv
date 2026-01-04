//! Custom shell completion generation with dynamic package name completion.
//!
//! This module extends the base clap completions with custom functions that
//! call `nxv complete-package` to provide dynamic package name suggestions.

use clap::CommandFactory;
use clap_complete::Shell;
use std::io::Write;

use crate::cli::Cli;

/// Generate shell completions with dynamic package name completion support.
pub fn generate_completions<W: Write>(shell: Shell, buf: &mut W) -> std::io::Result<()> {
    // First, generate base clap completions
    let mut base_completions = Vec::new();
    clap_complete::generate(shell, &mut Cli::command(), "nxv", &mut base_completions);
    let base = String::from_utf8_lossy(&base_completions);

    match shell {
        Shell::Bash => generate_bash(buf, &base),
        Shell::Zsh => generate_zsh(buf, &base),
        Shell::Fish => generate_fish(buf, &base),
        _ => {
            // For other shells, just use base completions
            buf.write_all(base.as_bytes())
        }
    }
}

/// Generate bash completions with dynamic package completion.
fn generate_bash<W: Write>(buf: &mut W, base: &str) -> std::io::Result<()> {
    // Write base completions first
    buf.write_all(base.as_bytes())?;

    // Add custom package completion function
    buf.write_all(
        br#"
# Dynamic package name completion for nxv
_nxv_complete_packages() {
    local cur="${COMP_WORDS[COMP_CWORD]}"
    local cmd="${COMP_WORDS[0]}"
    local packages
    # Use the actual command being completed (handles ./target/release/nxv, etc.)
    packages=$("$cmd" complete-package "$cur" --limit 100 2>/dev/null)
    if [[ -n "$packages" ]]; then
        COMPREPLY=($(compgen -W "$packages" -- "$cur"))
    fi
}

# Override completion for commands that take package names
_nxv_with_packages() {
    local cur prev words cword
    _init_completion || return

    # Check if we're completing a package name argument
    case "${words[1]}" in
        search|info|history)
            # First positional argument after the command is the package name
            if [[ $cword -eq 2 ]] && [[ "$cur" != -* ]]; then
                _nxv_complete_packages
                return
            fi
            ;;
    esac

    # Fall back to default completion
    _nxv
}

# Re-register the completion function
complete -F _nxv_with_packages nxv
"#,
    )
}

/// Generate zsh completions with dynamic package completion.
fn generate_zsh<W: Write>(buf: &mut W, base: &str) -> std::io::Result<()> {
    // Process base completions to remove the self-registration block at the end.
    // The base clap output ends with:
    //   if [ "$funcstack[1]" = "_nxv" ]; then
    //       _nxv "$@"
    //   else
    //       compdef _nxv nxv
    //   fi
    // We remove this entire block and register our enhanced version instead.
    // Use string replacement to target the specific block rather than filtering lines.
    let modified_base = base
        .replace(
            r#"if [ "$funcstack[1]" = "_nxv" ]; then
    _nxv "$@"
else
    compdef _nxv nxv
fi"#,
            "",
        )
        .trim_end()
        .to_string();

    buf.write_all(modified_base.as_bytes())?;

    // Add custom package completion function
    buf.write_all(
        br#"
# Dynamic package name completion for nxv
_nxv_packages() {
    local -a packages
    local cmd="${words[1]}"
    local prefix="${words[CURRENT]}"
    # Use the actual command being completed (handles ./target/release/nxv, etc.)
    packages=(${(f)"$("$cmd" complete-package "$prefix" --limit 100 2>/dev/null)"})
    if [[ ${#packages[@]} -gt 0 ]]; then
        _describe -t packages 'package' packages
    fi
}

# Enhance the existing completion with dynamic package names
# This function wraps _nxv to add package completion for relevant commands
_nxv_enhanced() {
    local curcontext="$curcontext" state line
    typeset -A opt_args

    # Check if we're completing a package name argument for search/info/history
    # words[1] = command, words[2] = subcommand, words[3] = package argument
    if [[ ${words[2]} == (search|info|history) ]] && [[ $CURRENT -eq 3 ]]; then
        _nxv_packages
        return
    fi

    # Fall back to default
    _nxv "$@"
}

# Register completions
# The #compdef nxv at the top handles 'nxv', but we override to use enhanced version
compdef _nxv_enhanced nxv
compdef _nxv_enhanced nxv-indexer
"#,
    )
}

/// Generate fish completions with dynamic package completion.
fn generate_fish<W: Write>(buf: &mut W, base: &str) -> std::io::Result<()> {
    // Write base completions first
    buf.write_all(base.as_bytes())?;

    // Add custom package completion function
    buf.write_all(
        br#"
# Dynamic package name completion for nxv
function __nxv_complete_packages
    set -l token (commandline -ct)
    set -l cmd (commandline -opc)[1]
    # Use the actual command being completed (handles ./target/release/nxv, etc.)
    $cmd complete-package "$token" --limit 100 2>/dev/null
end

# Add package completions for search, info, and history commands
complete -c nxv -n "__fish_seen_subcommand_from search" -f -a "(__nxv_complete_packages)"
complete -c nxv -n "__fish_seen_subcommand_from info" -f -a "(__nxv_complete_packages)"
complete -c nxv -n "__fish_seen_subcommand_from history" -f -a "(__nxv_complete_packages)"
"#,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generate_bash_completions() {
        let mut buf = Vec::new();
        generate_completions(Shell::Bash, &mut buf).unwrap();
        let output = String::from_utf8(buf).unwrap();

        // Should contain base completions
        assert!(output.contains("_nxv"));
        // Should contain custom package completion
        assert!(output.contains("_nxv_complete_packages"));
        assert!(output.contains("complete-package"));
    }

    #[test]
    fn test_generate_zsh_completions() {
        let mut buf = Vec::new();
        generate_completions(Shell::Zsh, &mut buf).unwrap();
        let output = String::from_utf8(buf).unwrap();

        // Should contain base completions
        assert!(output.contains("_nxv"));
        // Should contain custom package completion
        assert!(output.contains("_nxv_packages"));
        assert!(output.contains("complete-package"));
    }

    #[test]
    fn test_generate_fish_completions() {
        let mut buf = Vec::new();
        generate_completions(Shell::Fish, &mut buf).unwrap();
        let output = String::from_utf8(buf).unwrap();

        // Should contain base completions
        assert!(output.contains("complete -c nxv"));
        // Should contain custom package completion
        assert!(output.contains("__nxv_complete_packages"));
        assert!(output.contains("complete-package"));
    }
}
