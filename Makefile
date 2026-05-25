.PHONY: build test run-tui run-backend navd-update

GO_ENV = GOCACHE=$(CURDIR)/.cache/go-build GOMODCACHE=$(CURDIR)/.cache/go-mod
NAVD_LDFLAGS = -X main.sourceRoot=$(CURDIR)

build:
	cargo build --workspace
	cd tui && $(GO_ENV) go build -o ../target/debug/navd -ldflags "$(NAVD_LDFLAGS)" ./cmd/navd
	cd tui && $(GO_ENV) go build -o ../target/debug/nav ./cmd/nav

test:
	cargo test --workspace
	cd tui && $(GO_ENV) go test ./...

run-tui:
	cd tui && $(GO_ENV) go run ./cmd/nav

run-backend:
	cargo run -p nav-backend -- serve

navd-update:
	cd tui && $(GO_ENV) go run ./cmd/navd update
