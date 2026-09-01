use assert_cmd::cargo;
use std::fs;
use tempfile::TempDir;

fn create_project(sandbox: Option<bool>) -> (TempDir, TempDir, std::path::PathBuf) {
    let project = TempDir::new().unwrap();
    let cache = TempDir::new().unwrap();
    let config = project.path().join("rproject.toml");
    let sandbox = sandbox
        .map(|value| format!("sandbox = {value}\n"))
        .unwrap_or_default();
    fs::write(
        &config,
        format!(
            r#"[project]
name = "test-sandbox"
r_version = "4.5"
{sandbox}repositories = []
dependencies = []
"#
        ),
    )
    .unwrap();
    (project, cache, config)
}

fn rv_cmd(cache: &TempDir, config: &std::path::Path) -> assert_cmd::Command {
    let mut command = cargo::cargo_bin_cmd!();
    command.env("RV_CACHE_DIR", cache.path());
    command.args(["--config-file", config.to_str().unwrap()]);
    command
}

#[test]
fn environment_enables_sandbox_when_config_is_unset() {
    let (_project, cache, config) = create_project(None);
    let mut command = rv_cmd(&cache, &config);
    command.env("RV_SANDBOX_ENABLE", "1");
    command.args(["info", "--sandbox"]);

    let output = command.output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let sandbox = String::from_utf8_lossy(&output.stdout)
        .trim()
        .strip_prefix("sandbox: ")
        .unwrap()
        .to_string();
    assert!(!sandbox.is_empty());
    assert!(std::path::Path::new(&sandbox).is_dir());
}

#[test]
fn explicit_config_disables_environment_override() {
    let (_project, cache, config) = create_project(Some(false));
    let mut command = rv_cmd(&cache, &config);
    command.env("RV_SANDBOX_ENABLE", "1");
    command.args(["info", "--sandbox"]);

    let output = command.output().unwrap();
    assert!(output.status.success());
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "sandbox:");
}

#[test]
fn rv_run_repoints_library_and_uses_sandbox_in_library_paths() {
    let (project, cache, config) = create_project(Some(true));
    let mut command = rv_cmd(&cache, &config);
    command.current_dir(project.path());
    command.args([
        "run",
        "--no-sync",
        "-e",
        r#"cat(.Library, Sys.getenv("R_LIBS_SITE"), sep = "\n")"#,
    ]);

    let output = command.output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let lines = stdout.lines().collect::<Vec<_>>();
    assert_eq!(lines.len(), 2, "unexpected output: {stdout}");
    assert!(
        lines[0].contains("sandboxes"),
        "unexpected .Library: {stdout}"
    );
    assert!(
        lines[1].contains("rv/library") || lines[1].contains("rv\\library"),
        "project library missing: {stdout}"
    );
    assert!(
        lines[1].contains("sandboxes"),
        "sandbox missing from R_LIBS_SITE: {stdout}"
    );
}

#[test]
fn sync_isolates_r_build_and_install_subprocesses() {
    let project = TempDir::new().unwrap();
    let cache = TempDir::new().unwrap();
    let probe = project.path().join("probe");
    fs::create_dir_all(probe.join("R")).unwrap();
    fs::write(
        probe.join("DESCRIPTION"),
        r#"Package: sandboxprobe
Version: 0.1.0
Title: Verify rv Sandbox Isolation
Description: Records the R library paths observed while installing.
Authors@R: person("rv", "test", email = "rv@example.com", role = c("aut", "cre"))
License: MIT
Encoding: UTF-8
"#,
    )
    .unwrap();
    fs::write(probe.join("NAMESPACE"), "export(probe)\n").unwrap();
    fs::write(
        probe.join("R").join("probe.R"),
        r#"writeLines(
    c(.Library, Sys.getenv("R_LIBS_SITE")),
    Sys.getenv("RV_TEST_OUTPUT")
)
probe <- function() TRUE
"#,
    )
    .unwrap();

    let observed = project.path().join("observed-library-paths.txt");
    let observed_for_toml = observed.to_string_lossy().replace('\\', "/");
    let config = project.path().join("rproject.toml");
    fs::write(
        &config,
        format!(
            r#"[project]
name = "test-install-sandbox"
r_version = "4.5"
sandbox = true
repositories = []
dependencies = [{{ name = "sandboxprobe", path = "probe" }}]

[project.packages_env_vars]
sandboxprobe = {{ RV_TEST_OUTPUT = "{observed_for_toml}" }}
"#
        ),
    )
    .unwrap();

    let mut command = rv_cmd(&cache, &config);
    command.current_dir(project.path());
    command.arg("sync");
    let output = command.output().unwrap();
    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let observed = fs::read_to_string(observed).unwrap();
    let lines = observed.lines().collect::<Vec<_>>();
    assert_eq!(lines.len(), 2, "unexpected paths: {observed}");
    assert!(
        lines[0].contains("sandboxes"),
        "install-time .Library was not sandboxed: {observed}"
    );
    assert!(
        lines[1].contains("rv/library") || lines[1].contains("rv\\library"),
        "project library missing during install: {observed}"
    );
    assert!(
        lines[1].contains("sandboxes"),
        "sandbox missing from install-time R_LIBS_SITE: {observed}"
    );
}
