.PHONY: all build install clean run run-tui watch test lint fmt check help kill-zombies kill

# --- Config ---
RUST_BIN := ./target/release/melina
RUST_TUI := ./target/release/melina-tui
PREFIX   := /usr/local/bin

# --- Default ---
all: build ## Build release binaries

# --- Build ---
build: ## Build Rust binaries (release)
	cargo build --release

# --- Run ---
run: build ## Run CLI (one-shot snapshot)
	$(RUST_BIN)

run-tui: build ## Run TUI dashboard
	$(RUST_TUI)

watch: build ## Watch mode (refresh every 2s)
	$(RUST_BIN) --watch 2

json: build ## JSON output with teams
	$(RUST_BIN) --json --teams

kill-zombies: build ## Kill zombie teams + orphan tmux servers
	$(RUST_BIN) --kill-zombies

kill: build ## Kill process by PID (usage: make kill PID=12345)
	$(RUST_BIN) --kill $(PID)

# --- Install ---
install: build ## Symlink binaries to /usr/local/bin
	@echo "Symlinking melina -> $(PREFIX)/melina"
	ln -sf $(abspath $(RUST_BIN)) $(PREFIX)/melina
	ln -sf $(abspath $(RUST_TUI)) $(PREFIX)/melina-tui
	@echo "Done. Run 'melina' or 'melina-tui' from anywhere."

uninstall: ## Remove symlinks from /usr/local/bin
	rm -f $(PREFIX)/melina $(PREFIX)/melina-tui

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
