# Contributing to Compas

Thank you for your interest in contributing! This document covers how to get started.

## Getting Started

1. Fork and clone the repository
2. Install the Rust toolchain (`rustup`)
3. Install hooks: `make setup-hooks`
4. Build: `make build`
5. Run tests: `make test`

## Development Setup

Copy `.mcp.json.example` to `.mcp.json` and update paths to your local checkout. The dev config at `.compas/config.yaml` uses an isolated state directory.

```bash
make dashboard-dev   # Dashboard + worker on isolated dev DB
```

See [AGENTS.md](AGENTS.md) for module overview, architecture constraints, and the full development workflow.

## Before Submitting a PR

Run the full verification gate — this matches CI:

```bash
make fmt       # Apply formatting
make verify    # fmt-check + clippy + test + lint-md
```

All four checks must pass. CI runs on Linux (Ubuntu), so code that compiles on macOS may still fail CI due to platform-specific gating.

## Code Style

- Follow `rustfmt` defaults
- Use `Result<T, String>` for recoverable errors
- Use `unwrap()` only in tests
- All clippy warnings are errors (`-D warnings`)
- Test naming: `test_<component>_<feature>`

## Commit Guidelines

- Keep commits focused — one logical change per commit
- Write clear commit messages explaining *why*, not just *what*
- Update `CHANGELOG.md` under `[Unreleased]` for user-visible changes

## License

By contributing, you agree that your contributions will be licensed under the same terms as the project: [MIT](LICENSE-MIT) OR [Apache-2.0](LICENSE-APACHE), at the user's option.
