# Convenience targets. Everything here is a plain command you can also run by hand.

TAR_BASE_IRI ?= http://127.0.0.1:8080
export TAR_BASE_IRI

.PHONY: build test run seed ui ui-test fmt clean check

build: ui
	cargo build --release

check:
	cargo clippy --all-targets -- -D warnings || cargo build --all-targets

test:
	cargo test
	cd frontend && npm test

ui:
	cd frontend && npm install && npm run build

ui-test:
	cd frontend && npm test

# Serve the API and the built UI from one binary, the way the container does.
run: ui
	TAR_STATIC_DIR=frontend/dist cargo run -- serve

seed:
	cargo run -- seed --from ids-examples

fmt:
	cargo fmt

clean:
	cargo clean
	rm -rf frontend/dist frontend/node_modules data
