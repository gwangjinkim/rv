use std::path::{Path, PathBuf};
use std::process::Command;

use fs_err as fs;
use sha2::{Digest, Sha256};

use crate::Cache;
use crate::consts::{BASE_PACKAGES, RECOMMENDED_PACKAGES};
use crate::package::{Package, parse_description_file};
use crate::sync::LinkMode;

const SANDBOX_FORMAT_VERSION: &str = "2";
const SANDBOX_PROFILE_FILENAME: &str = ".rv-profile.R";
const SANDBOX_EMPTY_STARTUP_FILENAME: &str = ".rv-empty-startup";
pub(crate) const SANDBOX_LIBRARY_ENV_VAR_NAME: &str = "RV_SANDBOX_LIBRARY";

/// R startup code used by rv-managed subprocesses. R's `.Library` cannot be
/// overridden with `R_LIBS*`, so install/build subprocesses need a controlled
/// profile in addition to their explicit library search path.
const SANDBOX_PROFILE: &str = r#"local({
	sandbox <- Sys.getenv("RV_SANDBOX_LIBRARY", unset = "")
	if (!nzchar(sandbox) || !dir.exists(sandbox)) {
		stop("rv sandbox is enabled but its library is unavailable", call. = FALSE)
	}
	sandbox <- normalizePath(sandbox, winslash = "/", mustWork = TRUE)

	env <- baseenv()
	if (bindingIsLocked(".Library", env)) unlockBinding(".Library", env)
	assign(".Library", sandbox, envir = env)
	lockBinding(".Library", env)
	if (bindingIsLocked(".Library.site", env)) unlockBinding(".Library.site", env)
	assign(".Library.site", character(), envir = env)
	lockBinding(".Library.site", env)

	libraries <- strsplit(
		Sys.getenv("R_LIBS_SITE", unset = ""),
		.Platform$path.sep,
		fixed = TRUE
	)[[1]]
	libraries <- libraries[nzchar(libraries) & dir.exists(libraries)]
	.libPaths(c(libraries, sandbox), include.site = FALSE)
})
"#;

#[derive(Debug, Clone, Hash, PartialEq, Eq)]
pub struct Sandbox {
    path: PathBuf,
}

impl Sandbox {
    #[cfg(test)]
    pub(crate) fn for_test(path: PathBuf) -> Self {
        Self { path }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn profile_path(&self) -> PathBuf {
        self.path.join(SANDBOX_PROFILE_FILENAME)
    }

    fn empty_startup_path(&self) -> PathBuf {
        self.path.join(SANDBOX_EMPTY_STARTUP_FILENAME)
    }

    fn is_ready_at(path: &Path) -> bool {
        path.join(SANDBOX_PROFILE_FILENAME).is_file()
            && path.join(SANDBOX_EMPTY_STARTUP_FILENAME).is_file()
            && BASE_PACKAGES
                .iter()
                .all(|name| path.join(name).join("DESCRIPTION").is_file())
    }

    /// Isolate an R process from machine-level startup files and repoint its
    /// base system library to the rv sandbox.
    pub(crate) fn configure_r_startup(&self, command: &mut Command) {
        let empty = self.empty_startup_path();
        command
            .env(SANDBOX_LIBRARY_ENV_VAR_NAME, &self.path)
            .env("R_ENVIRON", &empty)
            .env("R_ENVIRON_USER", &empty)
            .env("R_PROFILE", self.profile_path())
            .env("R_PROFILE_USER", &empty);
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SandboxErrorKind {
    #[error("IO error: {error} ({path})")]
    File {
        error: std::io::Error,
        path: PathBuf,
    },
    #[error("base package `{name}` is missing or broken in the R library ({library})")]
    MissingBasePackage {
        name: &'static str,
        library: PathBuf,
    },
}

#[derive(Debug, thiserror::Error)]
#[error(transparent)]
#[non_exhaustive]
pub struct SandboxError {
    pub source: SandboxErrorKind,
}

impl SandboxError {
    /// For use in `map_err` on fs operations, capturing the path involved
    fn file(path: impl Into<PathBuf>) -> impl FnOnce(std::io::Error) -> Self {
        let path = path.into();
        move |error| Self {
            source: SandboxErrorKind::File { error, path },
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct SandboxPackages {
    packages: Vec<(PathBuf, Package)>,
}

impl SandboxPackages {
    pub fn sha(&self) -> String {
        let mut hasher = Sha256::new();
        hasher.update(format!("sandbox-format-{SANDBOX_FORMAT_VERSION}\n"));
        for (_, pkg) in &self.packages {
            hasher.update(format!("{}-{}\n", pkg.name, pkg.version.original));
        }
        let result = hex::encode(hasher.finalize());
        result[..10].to_string()
    }

    /// We start with symlinks but it can be an issue on Windows.
    /// If that doesn't work, then we try hardlinks and copy as last fallback
    pub fn materialize_to(&self, path: &Path) -> Result<(), SandboxError> {
        // Clear whatever is there first so we have a fresh sandbox
        if path.is_dir() {
            fs::remove_dir_all(path).map_err(SandboxError::file(path))?;
        }
        fs::create_dir_all(path).map_err(SandboxError::file(path))?;

        let mut mode = LinkMode::Symlink;
        for (lib_path, pkg) in &self.packages {
            let dest = path.join(&pkg.name);

            while let Err(error) = mode.link_package_dir(lib_path, &dest) {
                // A failed attempt can leave a partial result behind
                if let Ok(meta) = fs::symlink_metadata(&dest)
                    && meta.is_dir()
                {
                    fs::remove_dir_all(&dest).map_err(SandboxError::file(&dest))?;
                }

                let fallback = match mode {
                    LinkMode::Symlink => LinkMode::Hardlink,
                    LinkMode::Hardlink => LinkMode::Copy,
                    _ => {
                        return Err(SandboxError::file(dest)(std::io::Error::other(error)));
                    }
                };
                log::warn!(
                    "Could not {} {} into the sandbox: {error}. Falling back to {}.",
                    mode.name(),
                    pkg.name,
                    fallback.name()
                );
                mode = fallback;
            }
        }

        fs::write(path.join(SANDBOX_PROFILE_FILENAME), SANDBOX_PROFILE)
            .map_err(SandboxError::file(path.join(SANDBOX_PROFILE_FILENAME)))?;
        fs::write(path.join(SANDBOX_EMPTY_STARTUP_FILENAME), "").map_err(SandboxError::file(
            path.join(SANDBOX_EMPTY_STARTUP_FILENAME),
        ))?;

        Ok(())
    }
}

pub fn get_packages_to_copy(library: &Path) -> Result<SandboxPackages, SandboxError> {
    let mut pkgs = Vec::new();

    for entry in fs::read_dir(library).map_err(SandboxError::file(library))? {
        let entry = entry.map_err(SandboxError::file(library))?;

        let path = entry.path();
        let description_path = path.join("DESCRIPTION");
        if !path.is_dir() || !description_path.exists() {
            continue;
        }
        let name = entry.file_name().as_os_str().to_string_lossy().to_string();
        if !BASE_PACKAGES.contains(&name.as_str()) && !RECOMMENDED_PACKAGES.contains(&name.as_str())
        {
            continue;
        }
        let content =
            fs::read_to_string(&description_path).map_err(SandboxError::file(description_path))?;
        let Some(package) = parse_description_file(&content) else {
            continue;
        };

        pkgs.push((entry.path(), package));
    }

    pkgs.sort_by(|a, b| a.1.name.cmp(&b.1.name));
    Ok(SandboxPackages { packages: pkgs })
}

pub fn ensure_sandbox_exists(library: &Path, cache: &Cache) -> Result<Sandbox, SandboxError> {
    let content = get_packages_to_copy(library)?;
    let content_sha = content.sha();
    let (local, global) = cache.get_sandbox_paths(library);
    if let Some(g) = global {
        let path = g.join(&content_sha);
        if Sandbox::is_ready_at(&path) {
            return Ok(Sandbox { path });
        }
    }
    let sandbox_path = local.join(&content_sha);
    if Sandbox::is_ready_at(&sandbox_path) {
        return Ok(Sandbox { path: sandbox_path });
    }
    // Cache entries are created atomically, so an existing but incomplete
    // local entry is stale or damaged and can be regenerated safely.
    if let Ok(metadata) = fs::symlink_metadata(&sandbox_path) {
        if metadata.is_dir() && !metadata.file_type().is_symlink() {
            fs::remove_dir_all(&sandbox_path).map_err(SandboxError::file(&sandbox_path))?;
        } else {
            fs::remove_file(&sandbox_path).map_err(SandboxError::file(&sandbox_path))?;
        }
    }

    fs::create_dir_all(&local).map_err(SandboxError::file(&local))?;
    let tmp = tempfile::tempdir_in(&local).map_err(SandboxError::file(&local))?;
    content.materialize_to(tmp.path())?;

    for name in BASE_PACKAGES {
        if !tmp.path().join(name).join("DESCRIPTION").is_file() {
            return Err(SandboxError {
                source: SandboxErrorKind::MissingBasePackage {
                    name,
                    library: library.to_path_buf(),
                },
            });
        }
    }

    match fs::rename(tmp.path(), &sandbox_path) {
        Ok(()) => Ok(Sandbox { path: sandbox_path }),
        Err(error) => {
            if Sandbox::is_ready_at(&sandbox_path) {
                Ok(Sandbox { path: sandbox_path })
            } else {
                Err(SandboxError::file(sandbox_path)(error))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{SystemInfo, Version};

    fn add_package(library: &Path, name: &str, version: &str) {
        let package = library.join(name);
        fs::create_dir_all(&package).unwrap();
        fs::write(
            package.join("DESCRIPTION"),
            format!("Package: {name}\nVersion: {version}\n"),
        )
        .unwrap();
    }

    #[test]
    fn sandbox_contains_only_builtin_packages_and_startup_files() {
        let library = tempfile::tempdir().unwrap();
        add_package(library.path(), "base", "4.5.0");
        add_package(library.path(), "MASS", "7.3-65");
        add_package(library.path(), "pollutingPackage", "1.0.0");

        let packages = get_packages_to_copy(library.path()).unwrap();
        let output = tempfile::tempdir().unwrap();
        packages.materialize_to(output.path()).unwrap();

        assert!(output.path().join("base").join("DESCRIPTION").is_file());
        assert!(output.path().join("MASS").join("DESCRIPTION").is_file());
        assert!(!output.path().join("pollutingPackage").exists());
        assert!(output.path().join(SANDBOX_PROFILE_FILENAME).is_file());
        assert!(output.path().join(SANDBOX_EMPTY_STARTUP_FILENAME).is_file());
    }

    #[test]
    fn sandbox_is_cached_by_r_installation_and_package_versions() {
        let library = tempfile::tempdir().unwrap();
        for package in BASE_PACKAGES {
            add_package(library.path(), package, "4.5.0");
        }
        add_package(library.path(), "MASS", "7.3-65");
        add_package(library.path(), "pollutingPackage", "1.0.0");

        let cache_dir = tempfile::tempdir().unwrap();
        let version: Version = "4.5".parse().unwrap();
        let cache =
            Cache::new_in_dir(&version, SystemInfo::from_os_info(), cache_dir.path()).unwrap();

        let first = ensure_sandbox_exists(library.path(), &cache).unwrap();
        let second = ensure_sandbox_exists(library.path(), &cache).unwrap();
        assert_eq!(first, second);
        assert!(first.path().join("base").is_dir());
        assert!(first.path().join("MASS").is_dir());
        assert!(!first.path().join("pollutingPackage").exists());
        assert!(first.profile_path().is_file());
        assert!(first.empty_startup_path().is_file());

        fs::remove_file(first.profile_path()).unwrap();
        let repaired = ensure_sandbox_exists(library.path(), &cache).unwrap();
        assert_eq!(first, repaired);
        assert!(repaired.profile_path().is_file());
    }

    #[test]
    fn sandbox_profile_repoints_the_r_system_library() {
        let Ok(r) = which::which("R") else {
            return;
        };
        let output = tempfile::tempdir().unwrap();
        fs::write(
            output.path().join(SANDBOX_PROFILE_FILENAME),
            SANDBOX_PROFILE,
        )
        .unwrap();
        fs::write(output.path().join(SANDBOX_EMPTY_STARTUP_FILENAME), "").unwrap();
        let sandbox = Sandbox {
            path: output.path().to_path_buf(),
        };

        let mut command = Command::new(r);
        command
            .args([
                "--no-restore",
                "--no-save",
                "--no-init-file",
                "--silent",
                "--no-echo",
                "-e",
                "cat(normalizePath(.Library, winslash = '/'))",
            ])
            .env("R_DEFAULT_PACKAGES", "NULL")
            .env("R_ENABLE_JIT", "0")
            .env("R_LIBS_SITE", output.path());
        sandbox.configure_r_startup(&mut command);

        let result = command.output().unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&result.stdout),
            output.path().canonicalize().unwrap().to_string_lossy()
        );
    }
}
