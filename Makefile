SHELL := /bin/sh

CARGO ?= cargo
LAKE ?= lake
NPM ?= npm

DESKTOP_DIR := apps/gents-desktop
PROOFS_DIR := crates/gents/proofs
FUZZ_TIME ?= 30s

.DEFAULT_GOAL := help

.PHONY: help
help:
	@echo "Build:"
	@echo "  make build                 Build default Rust workspace members"
	@echo "  make build-cli             Build the Gents CLI"
	@echo "  make build-cli-headless    Build CLI without embedded Codex TUI"
	@echo "  make fast-dev-cli          Build CLI with lean dev debug artifacts"
	@echo "  make build-desktop         Build the Tauri Rust shell"
	@echo "  make build-desktop-ui      Build the desktop frontend"
	@echo
	@echo "Release (CI calls these; TARGET=<triple> optional, defaults to host):"
	@echo "  make release-cli           Build the release CLI (full features)"
	@echo "  make release-cli-headless  Build the release CLI without embedded Codex TUI"
	@echo "  make dist-cli              Build + package $(DIST_DIR)/gents-<triple>.tar.gz(+sha256)"
	@echo
	@echo "Measurements:"
	@echo "  make measure-build-graph   Report the normal CLI dependency graph"
	@echo "  make measure-release-cli   Build and report release binary metrics"
	@echo "  make measure-build-attribution  Cold build timing and linked-size attribution"
	@echo
	@echo "Checks:"
	@echo "  make fmt                   Format Rust and desktop UI code"
	@echo "  make fmt-check             Check Rust and desktop UI formatting"
	@echo "  make check-cli-headless    Check CLI without embedded Codex TUI"
	@echo "  make proofs                Build Lean proofs"
	@echo
	@echo "Tests:"
	@echo "  make test                  Run core Rust and CLI tests"
	@echo "  make test-agent            Run Gents runtime tests"
	@echo "  make test-agent-conformance  Run runtime conformance tests"
	@echo "  make test-agent-e2e        Run deterministic agent E2E tests"
	@echo "  make test-cli              Run CLI tests"
	@echo
	@echo "Desktop UI:"
	@echo "  make desktop-ui            Run full desktop UI suite"
	@echo "  make desktop-ui-qa-sweep   Run desktop QA sweep (format/build/unit/e2e/screenshots/fuzz)"
	@echo "  make desktop-ui-unit       Run desktop unit tests"
	@echo "  make desktop-ui-e2e        Run desktop Playwright journeys"
	@echo "  make desktop-ui-invariants Run desktop Playwright invariant checks"
	@echo "  make desktop-ui-screenshots  Capture stable desktop screenshot artifacts"
	@echo "  make desktop-ui-fuzz       Run desktop Bombadil smoke (FUZZ_TIME=$(FUZZ_TIME))"
	@echo "  make desktop-ui-fuzz-long  Run longer desktop Bombadil sweep"
	@echo "  make desktop-ui-agent      Start the JSONL browser driver for LLM agents"
	@echo "  make desktop-ui-visual     Run desktop visual baseline checks"
	@echo "  make desktop-ui-live-e2e   Run live browser-to-runtime desktop smoke"
	@echo "  make desktop-ui-live-e2e-real  Run live browser smoke against a configured real provider"
	@echo "  make desktop-native-preflight  Build frontend/Rust shell and print Tauri CLI version"
	@echo "  make desktop-native-dev    Launch the native Tauri dev app for manual QA"
	@echo "  make desktop-native-build  Build the native Tauri app bundle"
	@echo
	@echo "Live:"
	@echo "  make live-cli              Run live CLI smoke test"
	@echo "  make live-agent            Run ignored live runtime tests"
	@echo "  make live-desktop-smoke    Run live desktop smoke suites"
	@echo
	@echo "Worktrees:"
	@echo "  make worktree BRANCH=<branch> [DIR=<dest>] [BASE=<ref>]"
	@echo "                             Create a worktree with target/ and proofs/.lake"
	@echo "                             cloned from this checkout (APFS clonefile)"

.PHONY: worktree
worktree:
	@test -n "$(BRANCH)" || { echo "usage: make worktree BRANCH=<branch> [DIR=<dest>] [BASE=<ref>]" >&2; exit 2; }
	@WORKTREE_DIR="$(DIR)" WORKTREE_BASE="$(BASE)" scripts/worktree-bootstrap.sh "$(BRANCH)"

.PHONY: build build-cli build-cli-headless build-desktop build-desktop-ui
build:
	$(CARGO) build

build-cli:
	$(CARGO) build -p gents-cli

build-cli-headless:
	$(CARGO) build -p gents-cli --no-default-features

build-desktop:
	$(CARGO) build -p gents-desktop-tauri

build-desktop-ui:
	$(NPM) --prefix $(DESKTOP_DIR) run build

# ---- Release / packaging ----
# Produces the Linux release artifacts; the release workflow calls these so CI
# and local builds run the same commands. LTO, codegen-units and build
# parallelism are controlled by the caller's environment
# (CARGO_PROFILE_RELEASE_LTO, CARGO_BUILD_JOBS) — per-arch memory tuning lives
# in .github/workflows/release-linux.yml, not here.
TARGET ?=
DIST_DIR ?= dist
CARGO_TARGET_FLAG := $(if $(TARGET),--target $(TARGET),)
TARGET_TRIPLE := $(if $(TARGET),$(TARGET),$(shell rustc -Vv | awk '/^host:/ { print $$2 }'))
RELEASE_BIN := target/$(if $(TARGET),$(TARGET)/,)release/gents
RELEASE_ARTIFACT := gents-$(TARGET_TRIPLE)

.PHONY: release-cli release-cli-headless fast-dev-cli dist-cli measure-build-graph measure-release-cli measure-build-attribution
release-cli:
	$(CARGO) build -p gents-cli --release --locked $(CARGO_TARGET_FLAG)

release-cli-headless:
	$(CARGO) build -p gents-cli --release --locked --no-default-features $(CARGO_TARGET_FLAG)

fast-dev-cli:
	$(CARGO) build -p gents-cli --profile fast-dev --locked $(CARGO_TARGET_FLAG)

measure-build-graph:
	MEASURE_MODE=graph scripts/measure-gents-binary.sh

measure-release-cli:
	scripts/measure-gents-binary.sh

measure-build-attribution:
	scripts/measure-gents-build-attribution.sh

dist-cli: release-cli
	@rm -rf "$(DIST_DIR)/$(RELEASE_ARTIFACT)"
	@mkdir -p "$(DIST_DIR)/$(RELEASE_ARTIFACT)"
	cp "$(RELEASE_BIN)" "$(DIST_DIR)/$(RELEASE_ARTIFACT)/gents"
	chmod 0755 "$(DIST_DIR)/$(RELEASE_ARTIFACT)/gents"
	cp LICENSE "$(DIST_DIR)/$(RELEASE_ARTIFACT)/LICENSE"
	chmod 0644 "$(DIST_DIR)/$(RELEASE_ARTIFACT)/LICENSE"
	tar -C "$(DIST_DIR)" -czf "$(DIST_DIR)/$(RELEASE_ARTIFACT).tar.gz" "$(RELEASE_ARTIFACT)"
	cd "$(DIST_DIR)" && sha256sum "$(RELEASE_ARTIFACT).tar.gz" > "$(RELEASE_ARTIFACT).tar.gz.sha256"
	@rm -rf "$(DIST_DIR)/$(RELEASE_ARTIFACT)"
	@ls -lh "$(DIST_DIR)/$(RELEASE_ARTIFACT).tar.gz"*

.PHONY: fmt fmt-check check-cli-headless proofs
fmt:
	$(CARGO) fmt --all
	$(NPM) --prefix $(DESKTOP_DIR) run format

fmt-check:
	$(CARGO) fmt --all --check
	$(NPM) --prefix $(DESKTOP_DIR) run format:check

check-cli-headless:
	$(CARGO) check -p gents-cli --no-default-features

proofs:
	cd $(PROOFS_DIR) && $(LAKE) build

.PHONY: test test-agent test-agent-conformance test-agent-e2e test-cli
test: test-agent test-cli

test-agent:
	$(CARGO) test -p gents

test-agent-conformance:
	$(CARGO) test -p gents --test conformance

test-agent-e2e:
	$(CARGO) test -p gents --test e2e_lifecycle
	$(CARGO) test -p gents --test e2e_runtime
	$(CARGO) test -p gents --test e2e_subagent
	$(CARGO) test -p gents --test e2e_triggers

test-cli:
	$(CARGO) test -p gents-cli -- --nocapture --test-threads=1

.PHONY: desktop-ui desktop-ui-qa-sweep desktop-ui-unit desktop-ui-e2e desktop-ui-invariants desktop-ui-screenshots desktop-ui-fuzz desktop-ui-fuzz-long desktop-ui-agent desktop-ui-visual desktop-ui-live-e2e desktop-ui-live-e2e-real desktop-native-preflight desktop-native-dev desktop-native-build
desktop-ui:
	$(NPM) --prefix $(DESKTOP_DIR) run test:ui

desktop-ui-qa-sweep:
	$(NPM) --prefix $(DESKTOP_DIR) run test:ui:qa-sweep

desktop-ui-unit:
	$(NPM) --prefix $(DESKTOP_DIR) run test:ui:unit

desktop-ui-e2e:
	$(NPM) --prefix $(DESKTOP_DIR) run test:ui:e2e

desktop-ui-invariants:
	$(NPM) --prefix $(DESKTOP_DIR) run test:ui:invariants

desktop-ui-screenshots:
	$(NPM) --prefix $(DESKTOP_DIR) run test:ui:screenshots

desktop-ui-fuzz:
	$(NPM) --prefix $(DESKTOP_DIR) run test:ui:fuzz -- --time-limit $(FUZZ_TIME)

desktop-ui-fuzz-long:
	$(NPM) --prefix $(DESKTOP_DIR) run test:ui:fuzz:long

desktop-ui-agent:
	$(NPM) --prefix $(DESKTOP_DIR) run test:ui:agent

desktop-ui-visual:
	$(NPM) --prefix $(DESKTOP_DIR) run test:ui:visual

desktop-ui-live-e2e:
	$(NPM) --prefix $(DESKTOP_DIR) run test:ui:live:e2e

desktop-ui-live-e2e-real:
	$(NPM) --prefix $(DESKTOP_DIR) run test:ui:live:e2e:real

desktop-native-preflight:
	$(NPM) --prefix $(DESKTOP_DIR) run test:ui:native:preflight

desktop-native-dev:
	$(NPM) --prefix $(DESKTOP_DIR) run tauri -- dev

desktop-native-build:
	$(NPM) --prefix $(DESKTOP_DIR) run tauri -- build

.PHONY: live-cli live-agent live-desktop-smoke
live-cli:
	$(CARGO) test -p gents-cli --test cli_live standard_onboarding_live_demo_runs_real_conversation_with_filesystem_tools -- --ignored --nocapture --test-threads=1

live-agent:
	$(CARGO) test -p gents --test e2e_live -- --ignored --nocapture --test-threads=1

live-desktop-smoke:
	$(NPM) --prefix $(DESKTOP_DIR) run test:live:chat
	$(NPM) --prefix $(DESKTOP_DIR) run test:live:config
	$(NPM) --prefix $(DESKTOP_DIR) run test:live:operations
	$(NPM) --prefix $(DESKTOP_DIR) run test:live:interrupt
