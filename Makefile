# SoundWatch Lite
#
# Why this exists rather than plain `cargo build`:
#
# The binary carries an embedded Info.plist (see build.rs) so that macOS has a
# bundle identifier and a usage description to show when it asks for permission
# to observe system audio. Cargo's output is *linker-signed*, and a linker
# signature does not bind that section — `codesign -dvv` reports
# `Info.plist=not bound` and an identifier derived from the filename. TCC then
# has nothing to prompt with, the process tap is created anyway, and every
# sample it delivers is zero. Silently.
#
# Re-signing after the link fixes it. That is all this Makefile is for.
#
# SIGN_ID defaults to ad-hoc. Ad-hoc signatures identify the binary by its code
# hash, so *every rebuild is a new identity* and macOS asks for consent again.
# For anything you use daily, sign with a stable certificate instead:
#
#     make SIGN_ID="Developer ID Application: Your Name (TEAMID)"

CARGO   ?= cargo
SIGN_ID ?= -
PREFIX  ?= /usr/local
BIN     := soundwatch-lite

RELEASE := target/release/$(BIN)
DEBUG   := target/debug/$(BIN)

.PHONY: all build debug run test check fmt lint sign install uninstall clean probe hooks

all: build

## build — release binary, signed and ready to meter
build:
	$(CARGO) build --release
	@$(MAKE) --no-print-directory sign BIN_PATH=$(RELEASE)

## debug — debug binary, signed the same way
debug:
	$(CARGO) build
	@$(MAKE) --no-print-directory sign BIN_PATH=$(DEBUG)

## run — build and run against the live audio stack
run: debug
	@$(DEBUG)

## probe — is metering actually receiving samples?
probe: debug
	@$(DEBUG) --probe-tap

## sign — bind Info.plist into the code signature (BIN_PATH=...)
sign:
	@test -n "$(BIN_PATH)" || { echo "sign: set BIN_PATH"; exit 2; }
	@codesign --force --sign "$(SIGN_ID)" "$(BIN_PATH)"
	@codesign -dvv "$(BIN_PATH)" 2>&1 | grep -E '^(Identifier|Info.plist)' | sed 's/^/  /'

## test — the full suite, including tests that read real hardware
test:
	$(CARGO) test

## fmt — format in place
fmt:
	$(CARGO) fmt

## lint — clippy, warnings are errors
lint:
	$(CARGO) clippy --all-targets -- -D warnings

## hooks — refuse commits that do not build, lint and test clean
hooks:
	git config core.hooksPath .githooks
	@echo "pre-commit hook enabled (bypass with git commit --no-verify)"

## check — what CI runs
check:
	$(CARGO) fmt --check
	$(CARGO) clippy --all-targets -- -D warnings
	$(CARGO) test

## install — signed release binary into $(PREFIX)/bin
install: build
	install -d $(PREFIX)/bin
	install -m 755 $(RELEASE) $(PREFIX)/bin/$(BIN)
	@echo "installed $(PREFIX)/bin/$(BIN)"

uninstall:
	rm -f $(PREFIX)/bin/$(BIN)

clean:
	$(CARGO) clean
