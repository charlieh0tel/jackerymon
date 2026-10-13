# Every target says --workspace: this is a virtual workspace, so that is
# the default already, and saying it keeps the targets honest if a
# default-members is ever added.

CARGO ?= cargo

.PHONY: all build ci fmt fmt-check clippy test web-deps test-web deb

all: build

build:
	$(CARGO) build --workspace --all-targets

# What CI's Rust jobs check.
ci: fmt-check clippy test

fmt:
	$(CARGO) fmt --all

fmt-check:
	$(CARGO) fmt --all -- --check

clippy:
	$(CARGO) clippy --workspace --all-targets -- -D warnings

test:
	$(CARGO) test --workspace

# The browser tests' own Playwright and Chromium, into the test
# directory's node_modules rather than the home directory.
WEB_TESTS := crates/jackerymon-cli/tests/browser
web-deps:
	cd $(WEB_TESTS) && npm ci --no-fund --no-audit && npm run install-browser

# The daemon's page in a browser, against a faked API.  Needs
# `make web-deps` once.
test-web:
	cd $(WEB_TESTS) && npm test

deb:
	cargo deb -p jackerymon-cli
