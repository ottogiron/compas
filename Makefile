.PHONY: build release test fmt fmt-check clippy check clean verify worker dashboard mcp-server

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

# ── Quality gate (fmt-check + clippy + test) ─────────────────────────
verify: fmt-check clippy test

# ── Runtime convenience ──────────────────────────────────────────────
worker:
	cargo run --bin aster_orch -- worker --config .aster-orch/config.yaml

dashboard:
	cargo run --bin aster_orch -- dashboard --config .aster-orch/config.yaml

mcp-server:
	cargo run --bin aster_orch -- mcp-server --config .aster-orch/config.yaml
