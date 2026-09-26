# Repository Guidelines

## Project Structure & Module Organization

`src/main.rs` contains the host-side `bonsai` CLI, interactive wizard, file-editing commands, and unit tests; `src/flow.rs` is the pure generator for each tree's `src/sap.rs` (per-nutrient paths from `bonsai.toml` + the wiring). Generated projects live under `templates/<mcu>/<board>/`; Pico and ESP32 templates emit firmware, while the `rpi/*` boards (Zero W, Zero 2 W, Pi 5) emit a Linux application. These files are template data, not part of the host crate's build. `templates/_branch/` holds embedded branch scaffolds. Read `templates/README.md` before changing board templates or adding hardware support.

## Build, Test, and Development Commands

Run commands from the repository root. Prefix shell commands with `rtk`, as required by the repository's agent instructions.

- `rtk cargo build` builds the CLI.
- `rtk cargo run` launches the device and project wizard; it needs `cargo-generate` installed to create a project.
- `rtk cargo test` runs the unit tests in `src/main.rs`.
- `rtk cargo fmt --check` checks Rust formatting; `rtk cargo clippy` checks the host crate for lints.
- `rtk cargo install --path .` installs `bonsai` with the templates embedded in the binary.

Set `BONSAI_TEMPLATES=/path/to/templates` to try local template edits without rebuilding the CLI. Generated projects have their own build instructions; use each project's README for board-specific build, run, and deployment commands.

## Coding Style & Naming Conventions

Use Rust 2024 conventions and `cargo fmt` formatting (four-space indentation). Use `snake_case` for modules, files, functions, and branch names; use `PascalCase` for types and `Nutrient` variants. Keep generated-project marker lines such as `// bonsai:mod`, `// bonsai:start`, and `// bonsai:nutrient-arm` intact: CLI commands insert code at those markers. When adding a board, update both the hardware cascade in `src/main.rs` and its `templates/<mcu>/<board>/` directory.

## Testing Guidelines

Add focused `#[test]` cases to the existing test module in `src/main.rs`, using descriptive `snake_case` names. Run `rtk cargo test` after CLI or template changes. Template files are not compiled by the host crate, so also build a generated project when changing firmware code or target configuration. Extend the embedded-template completeness test when a board needs new files.

## Commit & Pull Request Guidelines

The repository currently has only an initial commit, so no recurring commit-message convention is established. Use a concise imperative subject that names the change. In pull requests, describe affected commands or boards, include test results, and note any hardware validation performed. Include a screenshot when changing the interactive wizard.
