.PHONY: build release test fmt fmt-check clippy check clean verify lint-md install worker dashboard dashboard-dev mcp-server setup-hooks

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

# ── Install ──────────────────────────────────────────────────────────
install:
	cargo install --path .
	@echo "Installed aster_orch to ~/.cargo/bin/"

# ── Setup ────────────────────────────────────────────────────────────
setup-hooks:
	mkdir -p .git/hooks
	ln -sf ../../scripts/hooks/pre-commit .git/hooks/pre-commit
	chmod +x scripts/hooks/pre-commit
	@echo "Pre-commit hook installed."

# ── Runtime convenience ──────────────────────────────────────────────
worker:
	cargo run --bin aster_orch -- worker --config .aster-orch/config.yaml

dashboard:
	cargo run --bin aster_orch -- dashboard --config .aster-orch/config.yaml

dashboard-dev:
	cargo run --bin aster_orch -- dashboard --with-worker --config .aster-orch/config.yaml

mcp-server:
	cargo run --bin aster_orch -- mcp-server --config .aster-orch/config.yaml
