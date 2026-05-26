.PHONY: build test run-tui run-backend navd-update

build:
	cargo build --workspace

test:
	cargo test --workspace
	cd tui && bun run typecheck

run-tui:
	cd tui && bun run start

run-backend:
	cargo run -p nav-backend -- serve-http

navd-update:
	cd tui && go run ./cmd/navd update
