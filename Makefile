# RepoSync – build, install, test and operate on macOS (and Linux).
#
#   make help        list all targets
#   make doctor      check prerequisites (read-only)
#   make install     build the release binary and install it to $(BINDIR)
#
# Variables can be overridden: make init ROOT=~/Code NAME="MacBook Pro"

SHELL      := /bin/bash
.DEFAULT_GOAL := help

PREFIX     ?= $(HOME)/.local
BINDIR     ?= $(PREFIX)/bin
CONFIG     ?= $(HOME)/.config/reposync/config.toml
ROOT       ?= $(HOME)/RepoSyncTest
NAME       ?= $(shell scutil --get ComputerName 2>/dev/null || hostname -s)
MODE       ?= two-way
PEER       ?=
HOST       ?=
CODE       ?=
PORT       ?= 47123
CARGO      ?= cargo
BIN        := target/release/reposync
RS         := $(BINDIR)/reposync --config $(CONFIG)
export BINDIR CONFIG

##@ Setup

.PHONY: help
help: ## Show this help
	@awk 'BEGIN {FS = ":.*##"} /^##@/ {printf "\n\033[1m%s\033[0m\n", substr($$0, 5)} \
	  /^[a-zA-Z0-9_-]+:.*##/ {printf "  \033[36m%-18s\033[0m %s\n", $$1, $$2}' $(MAKEFILE_LIST)
	@echo ""
	@echo "Variables: ROOT=$(ROOT) NAME=\"$(NAME)\" BINDIR=$(BINDIR) CONFIG=$(CONFIG)"
	@echo "Step by step: MAKE-HELP.md"

.PHONY: doctor
doctor: ## Check prerequisites, versions and network setup (read-only)
	@scripts/doctor.sh

.PHONY: toolchain
toolchain: ## Install the pinned Rust toolchain from rust-toolchain.toml
	@command -v rustup >/dev/null || { echo "rustup missing: curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh"; exit 1; }
	rustup show active-toolchain || rustup toolchain install "$$(sed -n 's/^channel *= *"\(.*\)"/\1/p' rust-toolchain.toml)"

.PHONY: tools
tools: ## Install dev tools (cargo-deny for security/licence checks)
	$(CARGO) install cargo-deny --locked

##@ Build

.PHONY: build
build: ## Build the optimised release binary (uses Cargo.lock exactly)
	$(CARGO) build --release --locked -p reposync-cli
	@$(BIN) --version

.PHONY: install
install: build ## Build and install reposync to $(BINDIR)
	@mkdir -p $(BINDIR)
	install -m 755 $(BIN) $(BINDIR)/reposync
	install -m 755 scripts/connect.sh $(BINDIR)/reposync-connect
	@echo "installed $$($(BINDIR)/reposync --version) → $(BINDIR)/reposync"
	@case ":$$PATH:" in *":$(BINDIR):"*) ;; *) echo "note: add $(BINDIR) to PATH: echo 'export PATH=\"$(BINDIR):\$$PATH\"' >> ~/.zshrc";; esac

.PHONY: app
app: ## Build the menu bar app build/RepoSync.app (Apple Silicon)
	scripts/build-app.sh

.PHONY: pkg
pkg: app ## Build the installer build/RepoSync-<version>-arm64.pkg
	scripts/build-pkg.sh

.PHONY: install-app
install-app: pkg ## Build and install RepoSync.app into /Applications (asks for your password)
	sudo installer -pkg build/RepoSync-$$(sed -n 's/^version *= *"\(.*\)"/\1/p' Cargo.toml | head -1)-arm64.pkg -target /

.PHONY: uninstall
uninstall: ## Remove the installed binary (configuration and data stay)
	rm -f $(BINDIR)/reposync $(BINDIR)/reposync-connect

.PHONY: version
version: ## Show installed, built and repository versions
	@echo "repository: $$(git describe --always --dirty --abbrev=10) on $$(git rev-parse --abbrev-ref HEAD)"
	@echo "installed : $$($(BINDIR)/reposync --version 2>/dev/null || echo 'not installed')"
	@echo "toolchain : $$(sed -n 's/^channel *= *"\(.*\)"/\1/p' rust-toolchain.toml) (pinned)"

.PHONY: update
update: ## Pull the latest version (fast-forward only), rebuild and reinstall
	@if [ -n "$$(git status --porcelain)" ]; then echo "working tree has local changes – commit or stash them first"; exit 1; fi
	git pull --ff-only
	$(MAKE) install
	@echo "restart a running daemon to use the new version (Ctrl-C, then: make run)"

.PHONY: clean
clean: ## Remove build artefacts
	$(CARGO) clean
	rm -rf build app/.build

##@ Quality

.PHONY: fmt
fmt: ## Format the code
	$(CARGO) fmt --all

.PHONY: lint
lint: ## rustfmt check + clippy with warnings as errors
	$(CARGO) fmt --all --check
	$(CARGO) clippy --workspace --all-targets --locked -- -D warnings

.PHONY: test
test: ## Run all unit, property and integration tests
	$(CARGO) test --workspace --locked

.PHONY: e2e
e2e: build ## End-to-end test: two daemons on this machine
	tests/e2e/two_daemons.sh $(BIN)

.PHONY: bench
bench: ## Benchmark with 100 000 files (in-process)
	$(CARGO) test --release --locked -p reposync-engine --test bench -- --ignored --nocapture

.PHONY: check
check: lint test audit ## Everything CI checks (lint, tests, audit)

##@ Dependencies

.PHONY: outdated
outdated: ## Report available toolchain/crate updates and security advisories
	@scripts/check-updates.sh

.PHONY: audit
audit: ## Security advisories, licences, sources (cargo-deny)
	@command -v cargo-deny >/dev/null || { echo "cargo-deny missing → make tools"; exit 1; }
	cargo deny check

.PHONY: update-deps
update-deps: ## Update Cargo.lock to the newest compatible crates, then test
	$(CARGO) update
	$(MAKE) lint test
	@echo "review with: git diff Cargo.lock"

.PHONY: update-toolchain
update-toolchain: ## Pin the latest stable Rust in rust-toolchain.toml, then test
	@v=$$(curl -fsSL https://static.rust-lang.org/dist/channel-rust-stable.toml | awk '/^\[pkg.rust\]/{f=1} f && /^version *=/{print; exit}' | sed -E 's/^version *= *"([0-9.]+).*/\1/'); \
	  test -n "$$v" || { echo "could not determine latest Rust"; exit 1; }; \
	  sed -i.bak -E "s/^channel *= *\".*\"/channel = \"$$v\"/" rust-toolchain.toml && rm -f rust-toolchain.toml.bak; \
	  echo "pinned Rust $$v"
	$(MAKE) lint test

##@ Run on two Macs (see docs/getting-started-mac.md)

.PHONY: connect
connect: ## One step on both Macs: choose directory, find the other Mac, pair, start (ROOT=…, ARGS=…)
	@scripts/connect.sh --config $(CONFIG) --port $(PORT) --mode $(MODE) --name "$(NAME)" $(if $(PEER),--peer $(PEER),) \
	  $(ARGS) $(if $(filter command line environment,$(origin ROOT)),"$(ROOT)",)

.PHONY: connect-help
connect-help: ## All options of reposync-connect
	@scripts/connect.sh --help

.PHONY: stop
stop: ## Stop the daemon / pairing wait on this Mac
	@scripts/connect.sh --stop

.PHONY: sandbox
sandbox: ## Create a safe test root $(ROOT) with a sample Git repo and folder
	@if [ -e "$(ROOT)" ] && [ -n "$$(ls -A "$(ROOT)" 2>/dev/null)" ]; then echo "$(ROOT) exists and is not empty – leaving it alone"; exit 0; fi
	mkdir -p "$(ROOT)/notes" "$(ROOT)/demo-repo"
	echo "# Notizen von $(NAME)" > "$(ROOT)/notes/README.md"
	cd "$(ROOT)/demo-repo" && git init -q && echo "console.log('hello')" > index.js && git add . && git commit -qm "initial commit from $(NAME)"
	@echo "sandbox ready: $(ROOT)"

.PHONY: init
init: ## Create the configuration (ROOT, NAME, MODE, optional PEER=host:port)
	$(BINDIR)/reposync --config $(CONFIG) init --root "$(ROOT)" --name "$(NAME)" --mode $(MODE) $(if $(PEER),--peer $(PEER),)

.PHONY: pair-listen
pair-listen: ## Show a pairing code and wait for the other Mac
	$(RS) pair listen

.PHONY: pair-connect
pair-connect: ## Pair with the listening Mac: make pair-connect HOST=other-mac.local CODE=123456
	@test -n "$(HOST)" -a -n "$(CODE)" || { echo "usage: make pair-connect HOST=<other-mac>.local CODE=<6 digits>"; exit 1; }
	$(RS) pair connect $(HOST) $(CODE)

.PHONY: run
run: ## Run the daemon in the foreground (recommended in Terminal.app for testing)
	REPOSYNC_LOG=$${REPOSYNC_LOG:-info} $(RS) daemon

.PHONY: status
status: ## Show the synchronisation state
	$(RS) status

.PHONY: log
log: ## Show the journal of the last hour
	$(RS) log --since 1h

.PHONY: service-install
service-install: ## Install the LaunchAgent (see the Local Network note in the docs first)
	$(RS) service install

.PHONY: service-uninstall
service-uninstall: ## Remove the LaunchAgent
	-launchctl bootout gui/$$(id -u)/com.reposync.daemon 2>/dev/null
	$(RS) service uninstall
