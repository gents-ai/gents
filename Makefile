SHELL := /bin/sh

CARGO ?= cargo
LAKE ?= lake
NPM ?= npm

DESKTOP_DIR := apps/desktop-tauri
PROOFS_DIR := crates/defra-agent/proofs
FUZZ_TIME ?= 30s

.DEFAULT_GOAL := help

.PHONY: help
help:
	@echo "Build:"
	@echo "  make build                 Build default Rust workspace members"
	@echo "  make build-cli             Build the defra-agent CLI"
	@echo "  make build-cli-headless    Build CLI without embedded Codex TUI"
	@echo "  make build-desktop         Build the Tauri Rust shell"
	@echo "  make build-desktop-ui      Build the desktop frontend"
	@echo
	@echo "Checks:"
	@echo "  make fmt                   Format Rust and desktop UI code"
	@echo "  make fmt-check             Check Rust and desktop UI formatting"
	@echo "  make check-cli-headless    Check CLI without embedded Codex TUI"
	@echo "  make proofs                Build Lean proofs"
	@echo
	@echo "Tests:"
	@echo "  make test                  Run core Rust and CLI tests"
	@echo "  make test-agent            Run defra-agent tests"
	@echo "  make test-agent-conformance  Run runtime conformance tests"
	@echo "  make test-agent-e2e        Run deterministic agent E2E tests"
	@echo "  make test-cli              Run CLI tests"
	@echo
	@echo "Desktop UI:"
	@echo "  make desktop-ui            Run full desktop UI suite"
	@echo "  make desktop-ui-unit       Run desktop unit tests"
	@echo "  make desktop-ui-e2e        Run desktop Playwright journeys"
	@echo "  make desktop-ui-fuzz       Run desktop Bombadil smoke (FUZZ_TIME=$(FUZZ_TIME))"
	@echo
	@echo "Live:"
	@echo "  make live-cli              Run live CLI smoke test"
	@echo "  make live-agent            Run ignored live runtime tests"
	@echo "  make live-desktop-smoke    Run live desktop smoke suites"

.PHONY: build build-cli build-cli-headless build-desktop build-desktop-ui
build:
	$(CARGO) build

build-cli:
	$(CARGO) build -p defra-agent-cli

build-cli-headless:
	$(CARGO) build -p defra-agent-cli --no-default-features

build-desktop:
	$(CARGO) build -p defra-agent-desktop-tauri

build-desktop-ui:
	$(NPM) --prefix $(DESKTOP_DIR) run build

.PHONY: fmt fmt-check check-cli-headless proofs
fmt:
	$(CARGO) fmt --all
	$(NPM) --prefix $(DESKTOP_DIR) run format

fmt-check:
	$(CARGO) fmt --all --check
	$(NPM) --prefix $(DESKTOP_DIR) run format:check

check-cli-headless:
	$(CARGO) check -p defra-agent-cli --no-default-features

proofs:
	cd $(PROOFS_DIR) && $(LAKE) build

.PHONY: test test-agent test-agent-conformance test-agent-e2e test-cli
test: test-agent test-cli

test-agent:
	$(CARGO) test -p defra-agent

test-agent-conformance:
	$(CARGO) test -p defra-agent --test conformance

test-agent-e2e:
	$(CARGO) test -p defra-agent --test e2e_lifecycle
	$(CARGO) test -p defra-agent --test e2e_runtime
	$(CARGO) test -p defra-agent --test e2e_subagent
	$(CARGO) test -p defra-agent --test e2e_triggers

test-cli:
	$(CARGO) test -p defra-agent-cli -- --nocapture --test-threads=1

.PHONY: desktop-ui desktop-ui-unit desktop-ui-e2e desktop-ui-fuzz
desktop-ui:
	$(NPM) --prefix $(DESKTOP_DIR) run test:ui

desktop-ui-unit:
	$(NPM) --prefix $(DESKTOP_DIR) run test:ui:unit

desktop-ui-e2e:
	$(NPM) --prefix $(DESKTOP_DIR) run test:ui:e2e

desktop-ui-fuzz:
	$(NPM) --prefix $(DESKTOP_DIR) run test:ui:fuzz -- --time-limit $(FUZZ_TIME)

.PHONY: live-cli live-agent live-desktop-smoke
live-cli:
	$(CARGO) test -p defra-agent-cli --test cli_live standard_onboarding_live_demo_runs_real_conversation_with_filesystem_tools -- --ignored --nocapture --test-threads=1

live-agent:
	$(CARGO) test -p defra-agent --test e2e_live -- --ignored --nocapture --test-threads=1

live-desktop-smoke:
	$(NPM) --prefix $(DESKTOP_DIR) run test:live:chat
	$(NPM) --prefix $(DESKTOP_DIR) run test:live:config
	$(NPM) --prefix $(DESKTOP_DIR) run test:live:operations
	$(NPM) --prefix $(DESKTOP_DIR) run test:live:interrupt
