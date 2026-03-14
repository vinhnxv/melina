.PHONY: all build install clean run run-cli watch test lint fmt check help kill-zombies kill

# --- Config ---
RUST_TUI := ./target/release/melina
RUST_CLI := ./target/release/melina-cli
PREFIX   := /usr/local/bin

# --- Default ---
all: build ## Build release binaries

# --- Build ---
build: ## Build Rust binaries (release)
	cargo build --release

# --- Run ---
run: build ## Run TUI dashboard (default)
	$(RUST_TUI)

run-cli: build ## Run CLI (one-shot snapshot)
	$(RUST_CLI)

watch: build ## Watch mode (refresh every 2s)
	$(RUST_CLI) --watch 2

json: build ## JSON output with teams
	$(RUST_CLI) --json --teams

kill-zombies: build ## Kill zombie teams + orphan tmux servers
	$(RUST_CLI) --kill-zombies

kill: build ## Kill process by PID (usage: make kill PID=12345)
	$(RUST_CLI) --kill $(PID)

# --- Install ---
install: build ## Symlink binaries to /usr/local/bin
	@echo "Symlinking melina -> $(PREFIX)/melina"
	ln -sf $(abspath $(RUST_TUI)) $(PREFIX)/melina
	ln -sf $(abspath $(RUST_CLI)) $(PREFIX)/melina-cli
	@echo "Done. Run 'melina' (TUI) or 'melina-cli' from anywhere."

uninstall: ## Remove symlinks from /usr/local/bin
	rm -f $(PREFIX)/melina $(PREFIX)/melina-cli

# --- Dev ---
check: ## Cargo check (fast compile check)
	cargo check

test: ## Run tests
	cargo test

lint: ## Run clippy
	cargo clippy -- -W warnings

fmt: ## Format code
	cargo fmt

# --- Clean ---
clean: ## Remove build artifacts
	cargo clean

# --- Help ---
help: ## Show this help
	@grep -E '^[a-zA-Z_-]+:.*## ' $(MAKEFILE_LIST) | sort | awk 'BEGIN {FS = ":.*## "}; {printf "\033[36m%-15s\033[0m %s\n", $$1, $$2}'
