# Loki. Two halves: a Rust core and a Swift app that links it.
#
# The app cannot link until the core is built, so every app target depends on `core`.

.DEFAULT_GOAL := help
CONFIG ?= debug

.PHONY: help
help:  ## Show this list
	@grep -hE '^[a-z-]+:.*?##' $(MAKEFILE_LIST) | sort | awk 'BEGIN{FS=":.*?## "}{printf "  \033[1m%-12s\033[0m %s\n", $$1, $$2}'

.PHONY: core
core:  ## Build the Rust core
	cargo build --workspace

.PHONY: cli
cli: core  ## Run the core on its own, no app. Needs a key in the environment
	cargo run -p loki-cli

.PHONY: app
app: core  ## Build Loki.app
	./scripts/build-app.sh $(CONFIG)

.PHONY: run
run: app  ## Build and launch Loki.app with the key from your shell
	./build/Loki.app/Contents/MacOS/Loki

.PHONY: xcode
xcode: core  ## Open the app in Xcode. Builds the core first so linking works
	@# Xcode links target/debug/libloki_ffi.a by path and has no dependency on the Rust sources,
	@# so building from Xcode alone links whatever core happens to be lying there. Building it
	@# here covers opening the project; scripts/xcode-prebuild.sh covers every build after that,
	@# and its header says how to install it as a scheme pre-action.
	xed app

.PHONY: test
test:  ## Run every test
	cargo test --workspace

.PHONY: check
check:  ## Everything CI would run. Fails loudly on the first problem
	cargo fmt --all --check
	cargo clippy --workspace --all-targets -- -D warnings
	cargo test --workspace
	@# From a clean .build, so an incremental cache cannot hide a link failure. That is exactly
	@# how a missing zlib link stayed invisible for several commits.
	rm -rf app/.build
	cd app && swift build
	@# The Swift side had no tests at all until a one-line change shipped a crash on almost every
	@# turn (B-66). `swift test` builds the app target too, so this is the gate for both.
	cd app && swift test
	@echo "check passed"

.PHONY: fmt
fmt:  ## Format the Rust code
	cargo fmt --all

.PHONY: clean
clean:  ## Remove build output
	cargo clean
	rm -rf app/.build build
