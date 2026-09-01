# rv

`rv` is a new way to manage and install your R packages in a reproducible, fast, and declarative way. 

## Documentation Site

Documentation site with examples, cookbooks, and more detailed instructions available at: [https://a2-ai.github.io/rv-docs/](https://a2-ai.github.io/rv-docs/)

## quick start/install

```
curl -sSL https://raw.githubusercontent.com/A2-ai/rv/refs/heads/main/scripts/install.sh | bash
rv --version
```

## How it works

`rv` has several top level commands to provide the user with as much flexibility as possible. The two primary commands are:
```
rv plan # detail what will occur if sync is run
rv sync # synchronize the library, config file, and lock file
```

The subsequent actions of these commands are controlled by a configuration file that specifies a desired project state by specifying the R version, repositories, and dependencies the project uses. Additionally, specific package and repository level customizations can be specified.

For example, a simple configuration file:
```toml
[project]
name = "my first rv project"
r_version = "4.4"

# any repositories, order matters
repositories = [
    { alias = "PPM", url = "https://packagemanager.posit.co/cran/latest" },
]

# top level packages to install
dependencies = [
    "dplyr",
    { name = "ggplot2", install_suggestions = true}
]
```

Running `rv sync` will synchronize the library, lock file, and configuration file by installing `dplyr`, `ggplot2`, any dependencies those packages require, and the suggested packages for `ggplot2`. Running `rv plan` will give you a preview of what `rv sync` will do.

Additional example projects with more configurations can be found in the [example_projects](example_projects)  directory of this repository.

## System library sandboxing

R always appends its system library (`.Library`) to `.libPaths()`. If that
library contains user-installed packages, they can leak into an otherwise
reproducible rv project. Setting only `R_LIBS`, `R_LIBS_SITE`, or `R_LIBS_USER`
does not solve this: none of those variables replaces `.Library`.

Sandboxing is opt-in and exposes only R's base and recommended packages from
the selected R installation:

```toml
[project]
sandbox = true
```

The configuration value takes precedence over the `RV_SANDBOX_ENABLE`
environment variable, including an explicit `sandbox = false`. When enabled,
rv creates a content-addressed system library and uses a controlled R startup
profile to repoint `.Library` and clear `.Library.site`. It also redirects the
site and user startup files so machine configuration cannot add host libraries
back into the process.

The same isolation is applied during activation, `rv run`, package bootstrap,
build, and installation. This matters because package R code can execute during
`R CMD INSTALL`; resolving dependencies reproducibly before installation is not
enough if that subprocess can still see packages installed on the host.
Sandboxes are cached by R installation, platform, and base/recommended package
versions, validated before reuse, and rebuilt if a local cache entry is
incomplete.

This compatibility-policy variant loads the normal project or user `.Rprofile`
when running `rv run`. An R profile is arbitrary startup code: it can change
`.libPaths()`, repositories, environment variables, options, the working
directory, or load packages before the requested script starts. Those machine-
or user-specific changes can mean that the lockfile and rv configuration no
longer determine the execution environment. Use `rv run --isolated ...` to
ignore `.Rprofile` for one reproducible invocation. Package bootstrap, build,
and installation remain isolated regardless of this `rv run` policy.

## Installation

See the [documentation site](https://a2-ai.github.io/rv-docs/) for installation instructions.

## Usage

See the [documentation site](https://a2-ai.github.io/rv-docs/) for usage guides and configuration reference.

## Contributing

### Getting started

To get started with the development of `rv`, you'll need:

- [Rust](https://rustup.rs/)
- and optionally [Just](https://github.com/casey/just)

After installing Rust, you can build the project by running:

```bash
just run <args>
// or
cargo run --features=cli --release -- ...
```

e.g. `just run sync` or `just run add --dry-run`.

If you'd like to install the current version of the project as a binary, you can run:

```bash
just install
// or
cargo install --path . --features cli
```

### Unit testing

Run the unit tests with:

```bash
just test
// or
cargo test --features=cli
```

### Snapshot testing

Snapshots require R version 4.5 (see `.github/workflows/ci.yaml` for current CI version).
