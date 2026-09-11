# rusty-broom

[![Actions Status](https://github.com/oriontvv/rusty-broom/actions/workflows/ci.yml/badge.svg?branch=master)](https://github.com/oriontvv/rusty-broom/actions/workflows/ci.yml) [![Coverage badge](https://raw.githubusercontent.com/oriontvv/rusty-broom/coverage/htmlcov/badges/flat.svg)](https://htmlpreview.github.io/?https://github.com/oriontvv/rusty-broom/coverage/htmlcov/index.html) [![dependency status](https://deps.rs/repo/github/oriontvv/rusty-broom/status.svg)](https://deps.rs/repo/github/oriontvv/rusty-broom) [![Crates.io](https://img.shields.io/crates/v/rusty-broom.svg)](https://crates.io/crates/rusty-broom)

<p align="center">
  <img src="assets/img/logo.webp" alt="Logo">
</p>

Frees up disk space by clearing build artifacts from projects you haven't worked on in a long time: `target`, `node_modules`, `.venv`, `build`, `Pods`, and so on.

<p align="center">
  <img src="assets/img/usage.webp" alt="Logo">
</p>

The primary metric is how recently the project was modified. The age is calculated based on the modification time of **the project's own files**. Artifacts, `.git`, and system/service directories are excluded from this calculation. This ensures that rebuilding the project or running `git gc` won't make an abandoned project appear "fresh," whereas editing source code or running `git pull` will.

The tool only deletes paths that Git itself considers ignored (`git check-ignore`). Committed `dist/` or `vendor/` directories will remain untouched, even if they are listed in the configuration file. Projects outside of a Git repository are labeled as `no-git`, and their artifacts are not verified—this is indicated in both the report and the user interface.

## Installation
* `cargo install rusty-broom`
* or download latest [build](https://github.com/oriontvv/rusty-broom/releases)

## Building

```sh
cargo build --release
./target/release/rusty-broom --help
```

## CLI

```sh
# Scan what's inside ~/dev (default threshold is 30 days)
rusty-broom scan ~/dev

# Projects untouched for six months, broken down by artifacts
rusty-broom scan ~/dev --older-than 6mo --long

# Only large projects of specific types, sorted by age in ascending order
rusty-broom scan ~/dev -o 1y -m 500M -t rust -t node --sort age

# Machine-readable output
rusty-broom scan ~/dev --json | jq '.totals'

# Delete: first preview the plan, then confirm
rusty-broom clean ~/dev -o 1y --dry-run
rusty-broom clean ~/dev -o 1y            # Will prompt for confirmation
rusty-broom clean ~/dev -o 1y --yes      # Without prompting

# List all supported project types
rusty-broom types
```

Useful flags: `--all` (ignore age threshold), `--query TEXT` (filter by path), `--nested` (show nested projects/workspace members), `--max-depth N`, `--no-git-check` (bypass Git validation—use at your own risk).

Running `clean` without the `--yes` flag in a non-interactive environment will not delete anything and will exit with an error; explicit confirmation is required.

## TUI

```sh
rusty-broom ~/dev          # Opens the interface when run without a subcommand
rusty-broom tui ~/dev -o 90d
```

Projects appear in the list as soon as they are found. Artifact sizes are calculated in the background by a worker pool and update progressively as they finish (uncalculated sizes are displayed with a tilde, e.g., `~4.2 GiB`).

| Key | Action |
|-----|--------|
| `j/k`, `↑/↓` | Move selection (`g`/`G` for top/bottom, `PgUp`/`PgDn` for page up/down) |
| `space` | Toggle selection mark |
| `a` / `A` | Mark all visible / Clear all marks |
| `c`, `Enter` | Clean marked (or current) projects—with confirmation |
| `o` / `O` | Increase/decrease the "untouched for longer than" threshold |
| `s` | Cycle sorting order: size → age → path → type |
| `/` | Filter by path or type |
| `v` | Toggle showing projects that have nothing to clean |
| `r` | Rescan from scratch |
| `?` | Show keybindings help |
| `q`, `Esc` | Exit |

## Configuration

```sh
rusty-broom config init    # Create a user config from the built-in template
rusty-broom config path    # Show which config file is currently in use
rusty-broom config show    # Display the final evaluated configuration
```

Lookup order: `--config FILE` → `./.rusty-broom.toml` → `~/.config/rusty-broom/config.toml` (on macOS: `~/Library/Application Support/rusty-broom/config.toml`) → built-in configuration.

Project types are configured as follows:

```toml
[settings]
older_than = "30d"
max_depth = 8
require_git_ignored = true

[[project_types]]
name = "rust"
markers = ["Cargo.toml"]      # Used to identify the project
artifacts = ["target"]        # Safe to delete

[[project_types]]
name = "python"
markers = ["pyproject.toml", "setup.py", "setup.cfg", "requirements.txt", "Pipfile", "poetry.lock"]
artifacts = [
    "**/__pycache__", ".venv", "venv", ".tox", ".nox",
    ".mypy_cache", ".pytest_cache", ".ruff_cache", ".ipynb_checkpoints",
    "build", "dist", "*.egg-info", "htmlcov",
]
```

`markers` and `artifacts` support wildcards (`*`, `?`), nested paths (`app/build.gradle`), and glob patterns (`**/__pycache__`). By default, a single matching marker is enough; setting `require_all_markers = true` enforces all of them (which is how the `unity` type is configured). Defining a `[[project_types]]` block with an existing name overrides the built-in type, while a new name adds it. Setting `enabled = false` deactivates the type.

Out of the box, it supports rust, node, python, maven, gradle, sbt, dotnet, go, cmake, swift, cocoapods, elixir, dart, php, ruby, haskell, zig, terraform, unity, and latex (the latter is disabled by default).

## Under the Hood

- **`scan`**: Traverses root directories, determines project types, resolves artifact patterns, and dates the project.
- **`engine`**: Comprises a scanner thread and a worker pool. It first filters paths via Git, then measures sizes, and finally handles deletion. All communication happens via channel events, ensuring the UI never freezes due to I/O operations, while the CLI simply processes the stream until completion.
- **`clean`**: Deletion with safety guards. The target must reside within the project root; the root cannot be the user's home directory or too close to the file system root; symbolic links are deleted as links and are never traversed.

Size is calculated based on actually allocated blocks (`st_blocks`), matching the behavior of the `du` command.

## Development

```sh
cargo test
cargo clippy --all-targets
bash scripts/fixture.sh   # Deploy test projects to \$TMPDIR for manual verification
```
