# Makefile for Gungnir FM
# Fast dual-pane Linux file manager written in Rust with Slint
# Companion GUI for Gungnir VFS
# Source: https://gitlab.com/exalted6165/gungnir-fm

# Toolchain
CARGO           ?= cargo
CARGO_FLAGS     ?=
RUSTFLAGS       ?=

# ISO/IEC 9899:2024 (C23) standard-compliant toolchain flags
CC              ?= gcc
CFLAGS          ?= -std=c23 -O2 -Wall -Wextra

# Installation directories (GNU standard conventions)
DESTDIR         ?=
PREFIX          ?= /usr/local
BINDIR          ?= $(PREFIX)/bin
DATADIR         ?= $(PREFIX)/share
APPLICATIONS_DIR?= $(DATADIR)/applications
ICONS_DIR       ?= $(DATADIR)/icons/hicolor/scalable/apps
METAINFO_DIR    ?= $(DATADIR)/metainfo

# Install & file utilities
INSTALL         ?= install
INSTALL_PROGRAM ?= $(INSTALL) -m 0755
INSTALL_DATA    ?= $(INSTALL) -m 0644
INSTALL_DIR     ?= $(INSTALL) -d -m 0755
RM              ?= rm -f
RMDIR           ?= rm -rf

# Target binaries
TARGET_DIR      ?= target
RELEASE_BIN      = $(TARGET_DIR)/release/gungnir-fm
DEBUG_BIN        = $(TARGET_DIR)/debug/gungnir-fm

.PHONY: all build release debug check test lint clippy fmt fmt-check doc \
        install install-user uninstall uninstall-user clean distclean \
        appimage flatpak gungnird help

# Default target
all: release

build: release

# Compile optimized release binary
release:
	@echo "==> Building Gungnir FM in release mode..."
	$(CARGO) build --release $(CARGO_FLAGS)

# Compile debug binary
debug:
	@echo "==> Building Gungnir FM in debug mode..."
	$(CARGO) build $(CARGO_FLAGS)

# Build companion gungnird daemon if sibling repository is available
gungnird:
	@echo "==> Building companion gungnird daemon..."
	./scripts/build-gungnird.sh

# Run workspace checks
check:
	@echo "==> Running workspace check..."
	$(CARGO) check --workspace $(CARGO_FLAGS)

# Run full workspace test suite
test:
	@echo "==> Running test suite across all workspace crates..."
	$(CARGO) test --workspace $(CARGO_FLAGS)

# Run Clippy linter
lint: clippy

clippy:
	@echo "==> Running Clippy across workspace..."
	$(CARGO) clippy --workspace --all-targets $(CARGO_FLAGS)

# Format codebase according to rustfmt.toml
fmt:
	@echo "==> Formatting code..."
	$(CARGO) fmt

# Check formatting compliance
fmt-check:
	@echo "==> Checking code formatting..."
	$(CARGO) fmt --check

# Build documentation
doc:
	@echo "==> Building workspace documentation..."
	$(CARGO) doc --workspace --no-deps $(CARGO_FLAGS)

# Install system-wide (or with DESTDIR for distribution packages)
install: release
	@echo "==> Installing Gungnir FM to $(DESTDIR)$(PREFIX)..."
	$(INSTALL_DIR) "$(DESTDIR)$(BINDIR)"
	$(INSTALL_PROGRAM) "$(RELEASE_BIN)" "$(DESTDIR)$(BINDIR)/gungnir-fm"
	$(INSTALL_DIR) "$(DESTDIR)$(APPLICATIONS_DIR)"
	$(INSTALL_DATA) "crates/ui/assets/gungnir-fm.desktop" "$(DESTDIR)$(APPLICATIONS_DIR)/gungnir-fm.desktop"
	$(INSTALL_DIR) "$(DESTDIR)$(ICONS_DIR)"
	$(INSTALL_DATA) "crates/ui/assets/gungnir-fm.svg" "$(DESTDIR)$(ICONS_DIR)/gungnir-fm.svg"
	$(INSTALL_DIR) "$(DESTDIR)$(METAINFO_DIR)"
	$(INSTALL_DATA) "packaging/flatpak/org.gungnir.GungnirFM.metainfo.xml" "$(DESTDIR)$(METAINFO_DIR)/org.gungnir.GungnirFM.metainfo.xml"
	@if [ -f ".build/gungnird/release/gungnird" ]; then \
		echo "==> Installing companion gungnird binary..."; \
		$(INSTALL_PROGRAM) ".build/gungnird/release/gungnird" "$(DESTDIR)$(BINDIR)/gungnird"; \
	fi
	@if [ -z "$(DESTDIR)" ]; then \
		if command -v update-desktop-database >/dev/null 2>&1; then \
			echo "==> Updating desktop database..."; \
			update-desktop-database "$(DESTDIR)$(APPLICATIONS_DIR)" 2>/dev/null || true; \
		fi; \
		if command -v gtk-update-icon-cache >/dev/null 2>&1; then \
			echo "==> Updating icon cache..."; \
			gtk-update-icon-cache -q -t -f "$(DESTDIR)$(DATADIR)/icons/hicolor" 2>/dev/null || true; \
		fi; \
	fi
	@echo "==> Installation complete: $(DESTDIR)$(BINDIR)/gungnir-fm"

# Install locally for the current user (no root required)
install-user:
	@$(MAKE) install PREFIX="$(HOME)/.local"
	@if [ -f "$(HOME)/.local/bin/gungnird" ] && [ -d /run/systemd/system ] && command -v systemctl >/dev/null 2>&1; then \
		echo "==> Configuring systemd user services for gungnird..."; \
		$(INSTALL_DIR) "$(HOME)/.config/systemd/user"; \
		$(INSTALL_DATA) "init/systemd/gungnird.service" "$(HOME)/.config/systemd/user/gungnird.service"; \
		$(INSTALL_DATA) "init/systemd/gungnird.socket" "$(HOME)/.config/systemd/user/gungnird.socket"; \
		systemctl --user daemon-reload 2>/dev/null || true; \
		systemctl --user enable --now gungnird.socket 2>/dev/null || true; \
		systemctl --user enable --now gungnird.service 2>/dev/null || true; \
	fi

# Uninstall from system or DESTDIR
uninstall:
	@echo "==> Uninstalling Gungnir FM from $(DESTDIR)$(PREFIX)..."
	$(RM) "$(DESTDIR)$(BINDIR)/gungnir-fm"
	$(RM) "$(DESTDIR)$(BINDIR)/gungnird"
	$(RM) "$(DESTDIR)$(APPLICATIONS_DIR)/gungnir-fm.desktop"
	$(RM) "$(DESTDIR)$(ICONS_DIR)/gungnir-fm.svg"
	$(RM) "$(DESTDIR)$(METAINFO_DIR)/org.gungnir.GungnirFM.metainfo.xml"
	@if [ -z "$(DESTDIR)" ]; then \
		if command -v update-desktop-database >/dev/null 2>&1; then \
			update-desktop-database "$(DESTDIR)$(APPLICATIONS_DIR)" 2>/dev/null || true; \
		fi; \
		if command -v gtk-update-icon-cache >/dev/null 2>&1; then \
			gtk-update-icon-cache -q -t -f "$(DESTDIR)$(DATADIR)/icons/hicolor" 2>/dev/null || true; \
		fi; \
	fi
	@echo "==> Uninstall complete!"

# Uninstall local user installation
uninstall-user:
	@if [ -d /run/systemd/system ] && command -v systemctl >/dev/null 2>&1; then \
		systemctl --user disable --now gungnird.service gungnird.socket 2>/dev/null || true; \
		$(RM) "$(HOME)/.config/systemd/user/gungnird.service" "$(HOME)/.config/systemd/user/gungnird.socket"; \
		systemctl --user daemon-reload 2>/dev/null || true; \
	fi
	@$(MAKE) uninstall PREFIX="$(HOME)/.local"

# Clean build artifacts
clean:
	@echo "==> Cleaning build artifacts..."
	$(CARGO) clean
	$(RMDIR) .build/AppDir .build/appdir.squashfs

# Deep clean including targets and packaging temporary files
distclean: clean
	@echo "==> Performing deep clean..."
	$(RMDIR) .build $(TARGET_DIR)

# Package targets
appimage: release
	@echo "==> Building standalone AppImage..."
	./packaging/appimage/build-appimage.sh

flatpak:
	@echo "==> Validating and packaging Flatpak..."
	./packaging/flatpak/build-flatpak.sh

# Display help information
help:
	@echo "Gungnir FM Build System"
	@echo "======================="
	@echo "Usage: make [TARGET] [PREFIX=/path] [DESTDIR=/path]"
	@echo ""
	@echo "Standard Targets:"
	@echo "  all (default)  Build optimized release binary ($(RELEASE_BIN))"
	@echo "  build          Alias for 'all'"
	@echo "  debug          Build unoptimized debug binary ($(DEBUG_BIN))"
	@echo "  check          Run cargo check on all workspace crates"
	@echo "  test           Run all unit and integration tests"
	@echo "  lint / clippy  Run cargo clippy with strict warnings"
	@echo "  fmt            Format Rust source files with rustfmt"
	@echo "  fmt-check      Verify formatting compliance"
	@echo "  doc            Generate workspace API documentation"
	@echo ""
	@echo "Installation Targets:"
	@echo "  install        Install system-wide (respects PREFIX and DESTDIR)"
	@echo "  uninstall      Remove system-wide installed files"
	@echo "  install-user   Install for current user under ~/.local (no sudo)"
	@echo "  uninstall-user Remove user installation from ~/.local"
	@echo ""
	@echo "Packaging & Extras:"
	@echo "  appimage       Package standalone portable AppImage"
	@echo "  flatpak        Validate Flatpak manifest and AppStream metadata"
	@echo "  gungnird       Build companion gungnird VFS daemon"
	@echo "  clean          Remove Cargo build artifacts"
	@echo "  distclean      Remove all build directories and packaging artifacts"
	@echo "  help           Show this help message"
	@echo ""
	@echo "Configuration Variables:"
	@echo "  PREFIX         Installation prefix (default: /usr/local)"
	@echo "  DESTDIR        Staging root directory for packaging (default: empty)"
	@echo "  CARGO_FLAGS    Extra flags passed to cargo (e.g., --offline, --locked)"
	@echo "  CC / CFLAGS    C compiler and flags (C23 compliance: -std=c23)"
