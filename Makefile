# PIXL — build & install
#
#   make              build (Release, native arch) and install AU + VST3 + CLAP, replacing old copies
#   make build        build only (bundles end up in build/wrapper/out)
#   make install      install already-built bundles
#   make universal    build a universal (arm64 + x86_64) release and install it
#   make validate     run Apple's auval on the installed Audio Unit
#   make test         run the Rust test suite
#   make uninstall    remove PIXL from the plug-in folders
#   make clean        remove build artifacts
#
#   CARGO_FLAGS="--no-default-features" make   builds without the custom editor (host generic UI)

SHELL := /bin/bash
.DEFAULT_GOAL := all

NAME          := PIXL
AU_TYPE       := aumu
AU_SUBTYPE    := PIXL
AU_MANUF      := PXLa
AU_DESC       := PIXL — Chiptune Synthesizer

ROOT          := $(abspath .)
BUILD_DIR     := $(ROOT)/build/wrapper
OUT_DIR       := $(BUILD_DIR)/out
UNIVERSAL_LIB := $(ROOT)/target/universal/release/libpixl.a

PLUGIN_DIR    := $(HOME)/Library/Audio/Plug-Ins
AU_DIR        := $(PLUGIN_DIR)/Components
VST3_DIR      := $(PLUGIN_DIR)/VST3
CLAP_DIR      := $(PLUGIN_DIR)/CLAP

HOST_ARCH     := $(shell uname -m)
ifeq ($(HOST_ARCH),arm64)
  RUST_HOST_TARGET := aarch64-apple-darwin
else
  RUST_HOST_TARGET := x86_64-apple-darwin
endif

ARCHS    ?= $(HOST_ARCH)
RUST_LIB ?= $(ROOT)/target/$(RUST_HOST_TARGET)/release/libpixl.a
CARGO_FLAGS ?=

PLISTBUDDY := /usr/libexec/PlistBuddy
empty :=
space := $(empty) $(empty)

.PHONY: all build rust wrapper postprocess install uninstall universal validate test clean

all: build install

build: rust wrapper postprocess

rust:
	@echo "==> Building Rust core ($(RUST_HOST_TARGET))"
	cargo build --release -p pixl-plugin --target $(RUST_HOST_TARGET) $(CARGO_FLAGS)

wrapper:
	@echo "==> Assembling AU / VST3 / CLAP bundles"
	@# Reconfigure when the library path or architectures change.
	cmake -S wrapper -B $(BUILD_DIR) -G Ninja \
		-DCMAKE_BUILD_TYPE=Release \
		-DCMAKE_OSX_ARCHITECTURES="$(subst $(space),;,$(strip $(ARCHS)))" \
		-DPIXL_RUST_LIB="$(RUST_LIB)" > /dev/null
	@# The Rust library is an imported target, so make sure a fresh one always relinks.
	@touch wrapper/pixl_entry.cpp
	cmake --build $(BUILD_DIR) --target $(NAME)_all

postprocess:
	@echo "==> Finalising bundle metadata and signatures"
	@AU_PLIST="$(OUT_DIR)/$(NAME).component/Contents/Info.plist"; \
	$(PLISTBUDDY) -c "Set :AudioComponents:0:description $(AU_DESC)" "$$AU_PLIST"; \
	$(PLISTBUDDY) -c "Delete :AudioComponents:0:tags" "$$AU_PLIST" 2>/dev/null || true; \
	$(PLISTBUDDY) -c "Add :AudioComponents:0:tags array" "$$AU_PLIST"; \
	$(PLISTBUDDY) -c "Add :AudioComponents:0:tags:0 string Synthesizer" "$$AU_PLIST"
	@for b in $(NAME).component $(NAME).vst3 $(NAME).clap; do \
		codesign --force --sign "$${CODESIGN_IDENTITY:--}" --timestamp=none "$(OUT_DIR)/$$b" > /dev/null 2>&1 || exit 1; \
	done
	@echo "    built: $(OUT_DIR)/{$(NAME).component,$(NAME).vst3,$(NAME).clap}"

install:
	@test -d "$(OUT_DIR)/$(NAME).component" || { echo "Nothing built yet — run 'make build' first"; exit 1; }
	@echo "==> Installing to $(PLUGIN_DIR) (replacing existing copies)"
	@mkdir -p "$(AU_DIR)" "$(VST3_DIR)" "$(CLAP_DIR)"
	@rm -rf "$(AU_DIR)/$(NAME).component" "$(VST3_DIR)/$(NAME).vst3" "$(CLAP_DIR)/$(NAME).clap"
	@cp -R "$(OUT_DIR)/$(NAME).component" "$(AU_DIR)/"
	@cp -R "$(OUT_DIR)/$(NAME).vst3" "$(VST3_DIR)/"
	@cp -R "$(OUT_DIR)/$(NAME).clap" "$(CLAP_DIR)/"
	@# Make macOS pick up the new Audio Unit without a reboot.
	@killall -9 AudioComponentRegistrar 2>/dev/null || true
	@echo "    AU   -> $(AU_DIR)/$(NAME).component"
	@echo "    VST3 -> $(VST3_DIR)/$(NAME).vst3"
	@echo "    CLAP -> $(CLAP_DIR)/$(NAME).clap"
	@echo "    Restart Logic Pro (or reset the plug-in manager) to load the new build."

uninstall:
	rm -rf "$(AU_DIR)/$(NAME).component" "$(VST3_DIR)/$(NAME).vst3" "$(CLAP_DIR)/$(NAME).clap"
	@killall -9 AudioComponentRegistrar 2>/dev/null || true

universal:
	@echo "==> Building universal Rust core (arm64 + x86_64)"
	rustup target add aarch64-apple-darwin x86_64-apple-darwin > /dev/null
	cargo build --release -p pixl-plugin --target aarch64-apple-darwin $(CARGO_FLAGS)
	cargo build --release -p pixl-plugin --target x86_64-apple-darwin $(CARGO_FLAGS)
	@mkdir -p $(dir $(UNIVERSAL_LIB))
	lipo -create -output $(UNIVERSAL_LIB) \
		target/aarch64-apple-darwin/release/libpixl.a \
		target/x86_64-apple-darwin/release/libpixl.a
	$(MAKE) wrapper postprocess install ARCHS="arm64 x86_64" RUST_LIB=$(UNIVERSAL_LIB)

validate:
	@killall -9 AudioComponentRegistrar 2>/dev/null || true
	auval -v $(AU_TYPE) $(AU_SUBTYPE) $(AU_MANUF)

test:
	cargo test --workspace --release

clean:
	cargo clean
	rm -rf build
