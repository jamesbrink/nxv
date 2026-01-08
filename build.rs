//! Build script for nxv.
//!
//! When the `indexer` feature is enabled, this links additional Nix C libraries
//! that are required by nix-bindings for expression evaluation.

fn main() {
    // Only link Nix libraries when the indexer feature is enabled
    #[cfg(feature = "indexer")]
    {
        // Link the Nix C API libraries for expression evaluation
        // These are required by nix-bindings for full functionality
        println!("cargo:rustc-link-lib=nixexprc");
        println!("cargo:rustc-link-lib=nixstorec");
        println!("cargo:rustc-link-lib=nixutilc");

        // Add library search paths from pkg-config if available
        if let Some(output) = std::process::Command::new("pkg-config")
            .args(["--libs-only-L", "nix-expr-c"])
            .output()
            .ok()
            .filter(|o| o.status.success())
        {
            for part in String::from_utf8_lossy(&output.stdout).split_whitespace() {
                if let Some(path) = part.strip_prefix("-L") {
                    println!("cargo:rustc-link-search={path}");
                }
            }
        }

        // Re-run if pkg-config configuration changes
        println!("cargo:rerun-if-env-changed=PKG_CONFIG_PATH");
    }
}
