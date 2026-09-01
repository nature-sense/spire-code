# spire-code monorepo — Rust core (crates/spire-code) + Swift UI (ui/swift).
#
# Targets:
#   make rust     — build the Rust crate (libspire_code.dylib + spire-core bin)
#   make swift    — build the Swift UI executable
#   make app      — build everything + assemble build/Spire.app (double-clickable)
#   make run      — assemble + launch the app
#   make test     — run Rust + Swift tests
#   make clean    — remove build artifacts

.PHONY: rust swift app run test clean

rust:
	cargo build --release -p spire-code

swift:
	cd ui/swift && swift build

app:
	@./build/assemble-app.sh

run: app
	@open ./build/Spire.app

test:
	cargo test -p spire-code
	cd ui/swift && swift test

clean:
	cargo clean
	rm -rf build/Spire.app
	cd ui/swift && swift package clean || true
