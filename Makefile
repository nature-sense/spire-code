# spire-code monorepo — Rust core (crates/spire-code) + Swift UI (ui/swift).
#
# Targets:
#   make rust      — build the Rust crate (libspire_code.dylib + spire-core bin)
#   make swift     — build the Swift UI executable
#   make app       — build everything + assemble build/Spire.app (double-clickable)
#   make run       — assemble + launch the app
#   make test      — run Rust + Swift tests (formatting is checked first)
#   make fmt       — format the Rust crates (this one + the spire-core sibling)
#   make fmt-check — fail if anything is unformatted
#   make clean     — remove build artifacts
#
# spire-core sits next to this repo as a path dependency (see Cargo.toml), so the
# fmt targets can reach it. Keeping both formatted stops the drift that made an
# earlier `cargo fmt` run reformat 43 files at once.

.PHONY: rust swift app run test fmt fmt-check clean

rust:
	cargo build --release -p spire-code

swift:
	cd ui/swift && swift build

app:
	@./build/assemble-app.sh

run: app
	@open ./build/Spire.app

fmt:
	cargo fmt
	cd ../spire-core && cargo fmt

fmt-check:
	cargo fmt --check
	cd ../spire-core && cargo fmt --check

test: fmt-check
	cargo test -p spire-code
	cd ui/swift && swift test

clean:
	cargo clean
	rm -rf build/Spire.app
	cd ui/swift && swift package clean || true
