# devload: Rust core (core/) + Swift shell (app/), one process.
#
#   make app        build/Devload.app, ad-hoc signed
#   make test       Rust tests, C ABI smoke test, Swift tests
#   make snapshots  PNGs of the icon and popover from sample data
#   make run        the app from the build folder, on a dev data folder (logs in build/run.log)

CARGO    ?= $(shell command -v cargo || echo $(HOME)/.cargo/bin/cargo)
CBINDGEN ?= $(shell command -v cbindgen || echo $(HOME)/.cargo/bin/cbindgen)
ROOT     := $(CURDIR)
LIBDIR   := $(ROOT)/core/target/release
APP      := build/Devload.app
DEV_DATA ?= $(ROOT)/build/dev-data

.PHONY: all core header abi-test swift app test snapshots run clean
all: app

core:
	$(CARGO) build --release --manifest-path core/Cargo.toml

header:
	cd core && $(CBINDGEN) --config cbindgen.toml --crate devload_core --output include/devload.h
	git diff --exit-code core/include/devload.h

abi-test: core
	mkdir -p build
	clang -std=c11 -Wall -Wextra -Werror -Icore/include core/tests/c/abi_smoke.c $(LIBDIR)/libdevload_core.a -o build/abi_smoke
	rm -rf build/abi-data && build/abi_smoke build/abi-data && rm -rf build/abi-data

# SwiftPM does not track the .a as a link input: without the rm, a new
# libdevload_core.a is not relinked. -dead_strip drops unused Rust code.
swift: core
	rm -f app/.build/release/Devload
	cd app && swift build -c release --product Devload -Xlinker -dead_strip

app: swift
	rm -rf $(APP)
	mkdir -p $(APP)/Contents/MacOS
	cp app/.build/release/Devload $(APP)/Contents/MacOS/Devload
	strip -x $(APP)/Contents/MacOS/Devload
	cp app/Info.plist $(APP)/Contents/Info.plist
	plutil -lint -s $(APP)/Contents/Info.plist
	codesign --force --sign - --identifier dev.devload.app $(APP)
	codesign --verify --strict $(APP)

test: core abi-test
	$(CARGO) test --release --manifest-path core/Cargo.toml
	rm -f app/.build/debug/DevloadPackageTests.xctest/Contents/MacOS/DevloadPackageTests
	cd app && swift test

snapshots: swift
	app/.build/release/Devload --snapshot build/snapshots

# Through LaunchServices, not the binary: run from a terminal, macOS 26
# counts the menu bar item as the terminal's and may hide it. Logs go to
# build/run.log; quitting devload ends the target.
run: app
	@echo "devload logs: build/run.log"
	open -n -W --env DEVLOAD_DATA_DIR=$(DEV_DATA) --env DEVLOAD_TRACE=1 \
		--stdout $(ROOT)/build/run.log --stderr $(ROOT)/build/run.log $(APP)

clean:
	rm -rf build app/.build core/target
