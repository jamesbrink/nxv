//! Package-spec parsing and structured Nix shell invocation.

use crate::db::queries::{PackageVersion, nix_attr_for_command};
use anyhow::{Context, Result, anyhow, bail};
use std::fmt;
use std::io;
use std::process::Command;
use std::str::FromStr;

/// One package query accepted by `nxv run`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageSpec {
    pub package: String,
    pub version: Option<String>,
}

impl PackageSpec {
    pub fn new(package: String, version: Option<String>) -> Self {
        Self { package, version }
    }
}

impl fmt::Display for PackageSpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.version {
            Some(version) => write!(f, "{}@{}", self.package, version),
            None => f.write_str(&self.package),
        }
    }
}

impl FromStr for PackageSpec {
    type Err = String;

    fn from_str(value: &str) -> std::result::Result<Self, Self::Err> {
        if value.is_empty() {
            return Err("package specification cannot be empty".to_string());
        }

        match value.rsplit_once('@') {
            Some(("", _)) => Err("package name before @ cannot be empty".to_string()),
            Some((_, "")) => Err("version after @ cannot be empty".to_string()),
            Some((package, version)) => {
                Ok(Self::new(package.to_string(), Some(version.to_string())))
            }
            None => Ok(Self::new(value.to_string(), None)),
        }
    }
}

/// A shell command represented as an executable, argv, and environment.
#[derive(Debug, PartialEq, Eq)]
pub struct ShellInvocation {
    program: &'static str,
    args: Vec<String>,
    allow_insecure: bool,
}

impl ShellInvocation {
    /// Build one shell invocation for all resolved packages.
    pub fn from_packages(packages: &[PackageVersion]) -> Result<Self> {
        if packages.is_empty() {
            bail!("cannot launch a shell without packages");
        }

        let allow_insecure = packages.iter().any(PackageVersion::is_insecure);
        let has_legacy = packages.iter().any(PackageVersion::predates_flakes);

        if has_legacy {
            let mut args = vec!["-p".to_string()];
            args.extend(
                packages
                    .iter()
                    .map(legacy_package_expression)
                    .collect::<Result<Vec<_>>>()?,
            );
            Ok(Self {
                program: "nix-shell",
                args,
                allow_insecure,
            })
        } else {
            let mut args = vec!["shell".to_string()];
            if allow_insecure {
                args.push("--impure".to_string());
            }
            args.extend(
                packages
                    .iter()
                    .map(flake_installable)
                    .collect::<Result<Vec<_>>>()?,
            );
            Ok(Self {
                program: "nix",
                args,
                allow_insecure,
            })
        }
    }

    /// Replace nxv with the selected Nix shell command on Unix.
    pub fn execute(self) -> Result<()> {
        let mut command = Command::new(self.program);
        command.args(&self.args);
        if self.allow_insecure {
            command.env("NIXPKGS_ALLOW_INSECURE", "1");
        }

        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            let error = command.exec();
            Err(execution_error(self.program, error))
        }

        #[cfg(not(unix))]
        {
            let status = command
                .status()
                .map_err(|error| execution_error(self.program, error))?;
            if !status.success() {
                bail!("`{}` exited with {}", self.program, status);
            }
            Ok(())
        }
    }
}

fn execution_error(program: &str, error: io::Error) -> anyhow::Error {
    if error.kind() == io::ErrorKind::NotFound {
        anyhow!(
            "cannot launch `{program}` because it was not found on PATH; install Nix from \
             https://nixos.org/download/ or ensure `{program}` is available on PATH"
        )
    } else {
        Err::<(), _>(error)
            .with_context(|| format!("failed to execute `{program}`"))
            .unwrap_err()
    }
}

fn validated_revision(package: &PackageVersion) -> Result<&str> {
    let revision = package.last_commit_hash.as_str();
    if revision.len() != 40 || !revision.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        bail!(
            "index returned invalid nixpkgs revision {:?} for {} {}; run `nxv sync` and retry",
            revision,
            package.attribute_path,
            package.version
        );
    }
    Ok(revision)
}

fn flake_installable(package: &PackageVersion) -> Result<String> {
    let revision = validated_revision(package)?;
    let (attribute, _) = nix_attr_for_command(&package.attribute_path);
    Ok(format!("nixpkgs/{revision}#{attribute}"))
}

fn legacy_package_expression(package: &PackageVersion) -> Result<String> {
    let revision = validated_revision(package)?;
    let (attribute, _) = nix_attr_for_command(&package.attribute_path);
    let import_args = legacy_import_args(package);
    Ok(format!(
        "(import (builtins.fetchTarball \"https://github.com/NixOS/nixpkgs/archive/{revision}.tar.gz\") {import_args}).{attribute}"
    ))
}

fn legacy_import_args(package: &PackageVersion) -> &'static str {
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    {
        if package.predates_flakes() {
            r#"{ system = "x86_64-darwin"; }"#
        } else {
            "{}"
        }
    }

    #[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
    {
        let _ = package;
        "{}"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};

    fn package(attribute_path: &str, hash: &str, timestamp: i64, insecure: bool) -> PackageVersion {
        PackageVersion {
            id: 1,
            name: attribute_path.to_string(),
            version: "1.0".to_string(),
            first_commit_hash: hash.to_string(),
            first_commit_date: Utc.timestamp_opt(timestamp, 0).unwrap(),
            last_commit_hash: hash.to_string(),
            last_commit_date: Utc.timestamp_opt(timestamp, 0).unwrap(),
            attribute_path: attribute_path.to_string(),
            description: None,
            license: None,
            homepage: None,
            maintainers: None,
            platforms: None,
            source_path: None,
            known_vulnerabilities: insecure.then(|| r#"["CVE-test"]"#.to_string()),
        }
    }

    #[test]
    fn parses_additional_package_specs() {
        assert_eq!(
            "nodejs@15".parse::<PackageSpec>().unwrap(),
            PackageSpec::new("nodejs".to_string(), Some("15".to_string()))
        );
        assert_eq!(
            "jq".parse::<PackageSpec>().unwrap(),
            PackageSpec::new("jq".to_string(), None)
        );
        assert!("@15".parse::<PackageSpec>().is_err());
        assert!("nodejs@".parse::<PackageSpec>().is_err());
    }

    #[test]
    fn builds_modern_multi_package_invocation() {
        let packages = [
            package(
                "python311",
                "0123456789abcdef0123456789abcdef01234567",
                1_700_000_000,
                false,
            ),
            package(
                "nodejs",
                "fedcba9876543210fedcba9876543210fedcba98",
                1_700_000_000,
                false,
            ),
        ];
        let invocation = ShellInvocation::from_packages(&packages).unwrap();

        assert_eq!(invocation.program, "nix");
        assert_eq!(
            invocation.args,
            [
                "shell",
                "nixpkgs/0123456789abcdef0123456789abcdef01234567#python311",
                "nixpkgs/fedcba9876543210fedcba9876543210fedcba98#nodejs"
            ]
        );
        assert!(!invocation.allow_insecure);
    }

    #[test]
    fn builds_legacy_invocation_for_mixed_eras() {
        let packages = [
            package(
                "python27",
                "0123456789abcdef0123456789abcdef01234567",
                1_500_000_000,
                false,
            ),
            package(
                "nodejs",
                "fedcba9876543210fedcba9876543210fedcba98",
                1_700_000_000,
                false,
            ),
        ];
        let invocation = ShellInvocation::from_packages(&packages).unwrap();

        assert_eq!(invocation.program, "nix-shell");
        assert_eq!(invocation.args[0], "-p");
        assert!(invocation.args[1].contains("0123456789abcdef"));
        assert!(invocation.args[1].ends_with(".python27"));
        assert!(invocation.args[2].contains("fedcba9876543210"));
        assert!(invocation.args[2].ends_with(".nodejs"));
        if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
            assert!(invocation.args[1].contains(r#"{ system = "x86_64-darwin"; }"#));
            assert!(!invocation.args[2].contains("x86_64-darwin"));
        }
    }

    #[test]
    fn enables_insecure_packages_and_quotes_attribute_segments() {
        let packages = [package(
            "aspellDicts.or",
            "0123456789abcdef0123456789abcdef01234567",
            1_700_000_000,
            true,
        )];
        let invocation = ShellInvocation::from_packages(&packages).unwrap();

        assert_eq!(invocation.program, "nix");
        assert_eq!(invocation.args[1], "--impure");
        assert_eq!(
            invocation.args[2],
            "nixpkgs/0123456789abcdef0123456789abcdef01234567#aspellDicts.\"or\""
        );
        assert!(invocation.allow_insecure);
    }

    #[test]
    fn rejects_invalid_index_revisions_before_launch() {
        let package = package("python311", "not-a-revision", 1_700_000_000, false);
        let error = ShellInvocation::from_packages(&[package]).unwrap_err();

        assert!(error.to_string().contains("invalid nixpkgs revision"));
        assert!(error.to_string().contains("nxv sync"));
    }

    #[test]
    fn missing_nix_error_is_actionable() {
        let error = execution_error("nix", io::Error::new(io::ErrorKind::NotFound, "missing"));
        let message = error.to_string();

        assert!(message.contains("not found on PATH"));
        assert!(message.contains("install Nix"));
    }
}
