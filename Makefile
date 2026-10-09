# Developer and CI entry points for the Cargo workspace. Plain cargo/rustup
# underneath, so `make` needs no tools beyond the Rust toolchain
# (rust-toolchain.toml) plus cargo-deny for `deny` (`mise install`, or
# `cargo install --locked cargo-deny`).
#
#   make check   everything CI runs: lint, test, doc, deny, msrv, oracle
#   make lint    fmt-check + clippy (what the pre-commit hook runs, roughly)

CARGO ?= cargo
# MSRV: single source of truth is `rust-version` in the workspace Cargo.toml.
MSRV := $(shell sed -n 's/^rust-version *= *"\(.*\)"/\1/p' Cargo.toml)
ORACLE := --manifest-path oracle/rust_tx3/Cargo.toml --target-dir target/oracle

.DEFAULT_GOAL := help
.PHONY: help check lint fmt fmt-check clippy test test-release test-ignored doc deny deny-offline msrv oracle

help: ## list targets
	@grep -E '^[a-z-]+:.*## ' $(MAKEFILE_LIST) | awk -F':.*## ' '{printf "  %-14s %s\n", $$1, $$2}'

check: lint test doc deny msrv oracle ## everything CI runs

lint: fmt-check clippy ## formatting check + clippy -D warnings

fmt: ## format all Rust sources (workspace + oracle helper)
	$(CARGO) fmt --all
	$(CARGO) fmt --manifest-path oracle/rust_tx3/Cargo.toml

fmt-check: ## verify formatting
	$(CARGO) fmt --all --check
	$(CARGO) fmt --manifest-path oracle/rust_tx3/Cargo.toml --check

clippy: ## clippy on all targets, warnings are errors
	$(CARGO) clippy --workspace --all-targets --locked -- -D warnings

test: ## unit, integration and doc tests ([profile.test] is opt-level 2; #[ignore]d tests excluded)
	$(CARGO) test --workspace --locked

test-release: ## the same tests in release (faster PHY loopbacks)
	$(CARGO) test --workspace --locked --release

test-ignored: ## heavy #[ignore]d stress/throughput tests, release (not in CI)
	$(CARGO) test --workspace --locked --release -- --ignored

doc: ## rustdoc with warnings as errors
	RUSTDOCFLAGS="-D warnings" $(CARGO) doc --workspace --no-deps --locked

deny: ## cargo-deny: advisories, bans, licenses, sources
	$(CARGO) deny --locked check

deny-offline: ## cargo-deny without the advisory database fetch (pre-commit)
	$(CARGO) deny --locked check bans licenses sources

msrv: ## type-check every target with the MSRV toolchain (rustup toolchain install $(MSRV))
	$(CARGO) +$(MSRV) check --workspace --all-targets --locked --target-dir target/msrv

oracle: ## build-check and clippy the stand-alone oracle/rust_tx3 helper
	$(CARGO) clippy $(ORACLE) --all-targets --locked -- -D warnings
