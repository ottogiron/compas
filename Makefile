.PHONY: build release test fmt fmt-check clippy check clean verify lint-md install worker dashboard dashboard-dev dashboard-standalone mcp-server setup-hooks changelog

# ── Build ────────────────────────────────────────────────────────────
build:
	cargo build

release:
	cargo build --release

check:
	cargo check

clean:
	cargo clean

# ── Quality ──────────────────────────────────────────────────────────
test:
	cargo test

fmt:
	cargo fmt --all

fmt-check:
	cargo fmt --all -- --check

clippy:
	cargo clippy --all-targets -- -D warnings

lint-md:
	npx markdownlint-cli2 "**/*.md"

# ── Quality gate (fmt-check + clippy + test + markdown lint) ─────────
verify: fmt-check clippy test lint-md

# ── Changelog ────────────────────────────────────────────────────────
changelog:
	@changie batch auto --dry-run

# ── Install ──────────────────────────────────────────────────────────
install:
	cargo install --path .
	@echo "Installed compas to ~/.cargo/bin/"

# ── Setup ────────────────────────────────────────────────────────────
setup-hooks:
	mkdir -p .git/hooks
	ln -sf ../../scripts/hooks/pre-commit .git/hooks/pre-commit
	chmod +x scripts/hooks/pre-commit
	@echo "Pre-commit hook installed."

# ── Runtime convenience ──────────────────────────────────────────────
worker:
	cargo run --bin compas -- worker --config .compas/config.yaml

dashboard:
	cargo run --bin compas -- dashboard --config .compas/config.yaml

dashboard-dev:
	cargo run --bin compas -- dashboard --config .compas/config.yaml

dashboard-standalone:
	cargo run --bin compas -- dashboard --standalone --config .compas/config.yaml

mcp-server:
	cargo run --bin compas -- mcp-server --config .compas/config.yaml
