# =============================================================================
#  SedirOS - developer workflow
#  Works with GNU make >= 3.81 (macOS /usr/bin/make) on Linux and macOS.
#  On macOS the build scripts re-run themselves inside Docker.
# =============================================================================
SHELL := /bin/bash
.DEFAULT_GOAL := help

include config/sedir.conf

UNAME_S := $(shell uname -s)
UNAME_M := $(shell uname -m)
ARCH := $(if $(filter x86_64,$(UNAME_M)),amd64,$(if $(filter arm64 aarch64,$(UNAME_M)),arm64,unsupported))

BUILD      := build
PROFILE_TAG := $(if $(filter vm,$(PROFILE)),,-$(PROFILE))
ROOTFS_TAR := $(BUILD)/sedir-rootfs-$(RELEASE)$(PROFILE_TAG)-$(ARCH).tar.zst
IMAGE      := $(BUILD)/sedir-$(RELEASE)$(PROFILE_TAG)-$(ARCH).qcow2
# The ISO (live session + installer) is always built from the install profile.
INSTALL_ROOTFS_TAR := $(BUILD)/sedir-rootfs-$(RELEASE)-install-$(ARCH).tar.zst
ISO        := $(BUILD)/sedir-$(RELEASE)-install-$(ARCH).iso
CLI        := sedir-cli/target/release/sedir
pkg_version = $(shell sed -n 's/^Version: *//p' packages/$(1)/DEBIAN/control)
DEBS       := $(BUILD)/debs/sedir-base-files_$(RELEASE)_all.deb \
              $(BUILD)/debs/sedir-hardening_$(call pkg_version,sedir-hardening)_all.deb \
              $(BUILD)/debs/sedir-boot-hardening_$(call pkg_version,sedir-boot-hardening)_all.deb \
              $(BUILD)/debs/sedir-guest_$(call pkg_version,sedir-guest)_all.deb \
              $(BUILD)/debs/sedir-archive-keyring_$(call pkg_version,sedir-archive-keyring)_all.deb
# Only in the install profile (live ISO + installer).
INSTALLER_BIN := $(BUILD)/bin/sedir-install-$(ARCH)
LIVE_DEB      := $(BUILD)/debs/sedir-live_$(call pkg_version,sedir-live)_all.deb
INSTALLER_DEB := $(BUILD)/debs/sedir-install_$(call pkg_version,sedir-install)_$(ARCH).deb
INSTALL_DEBS  := $(LIVE_DEB) $(INSTALLER_DEB)
# In every image (vm and install profile).
AUDIT_BIN     := $(BUILD)/bin/sedir-audit-$(ARCH)
AUDIT_DEB     := $(BUILD)/debs/sedir-audit_$(call pkg_version,sedir-audit)_$(ARCH).deb

# Linux builds need root (debootstrap/chroot, file ownership); -E keeps SEDIR_* env.
SUDO := $(if $(filter Linux,$(UNAME_S)),$(if $(filter-out 0,$(shell id -u)),sudo -E,),)

export SEDIR_RELEASE       := $(RELEASE)
# SUITE is derived from RELEASE by the scripts; set it only for sid builds.
ifdef SUITE
export SEDIR_SUITE         := $(SUITE)
endif
export SEDIR_USER          := $(GUEST_USER)
export SEDIR_PROFILE       := $(PROFILE)
# No SSH key is baked into any image: vm images get the sedir CLI's key at
# boot (systemd credential), installed systems get keys from the installer.
export SEDIR_MAX_BOOT_SECS := $(MAX_BOOT_SECS)
export SEDIR_MAX_RAM_MB    := $(MAX_RAM_MB)

# Inputs that change the rootfs (config/sedir.conf only holds make variables).
ROOTFS_INPUTS := scripts/build_rootfs.sh scripts/lib/release.sh $(DEBS) $(AUDIT_DEB) $(if $(filter install,$(PROFILE)),$(INSTALL_DEBS)) $(filter-out config/sedir.conf config/iso/%,$(shell find config -type f))
PKG_INPUTS    := scripts/build_packages.sh scripts/lib/release.sh config/sedir.conf $(shell find packages -type f ! -name .DS_Store)
CLI_INPUTS    := sedir-cli/Cargo.toml $(shell find sedir-cli/src -name '*.rs')
SHELL_SCRIPTS := $(wildcard scripts/*.sh scripts/lib/*.sh tests/*.sh tests/lib/*.sh)

.PHONY: help deb rootfs image build iso iso-run iso-test install-test apt-test cli installer audit install test lint fmt launch shell stop clean

help: ## Show this help
	@echo "SedirOS $(RELEASE) ($(ARCH), profile $(PROFILE)) - usage: make <target> [RELEASE=..] [PROFILE=vm|install] [VM=..]"
	@echo
	@grep -hE '^[a-z-]+:.*## ' $(MAKEFILE_LIST) | awk -F':.*## ' '{printf "  \033[1m%-10s\033[0m %s\n", $$1, $$2}'

# --- artifacts ---------------------------------------------------------------

# One build_packages.sh run produces every .deb (grouped target emulation for make 3.81).
$(firstword $(DEBS)): $(PKG_INPUTS)
	scripts/build_packages.sh
$(wordlist 2,$(words $(DEBS)),$(DEBS)): $(firstword $(DEBS))
	@test -f $@ || { echo "$@ missing after build_packages.sh"; exit 1; }

$(ROOTFS_TAR): $(ROOTFS_INPUTS)
	$(SUDO) scripts/build_rootfs.sh --clean

$(IMAGE): $(ROOTFS_TAR) scripts/build_image.sh
	$(SUDO) scripts/build_image.sh --rootfs $(ROOTFS_TAR) --size $(IMAGE_SIZE)

$(INSTALLER_BIN): sedir-install/Cargo.toml sedir-install/Cargo.lock $(shell find sedir-install/src -name '*.rs') scripts/build_installer.sh
	scripts/build_installer.sh

# sedir-live is built with the other arch-independent packages; the
# installer package needs its binary first.
$(LIVE_DEB): $(firstword $(DEBS))
	@test -f $@ || { echo "$@ missing after build_packages.sh"; exit 1; }
$(INSTALLER_DEB): $(PKG_INPUTS) $(INSTALLER_BIN)
	scripts/build_packages.sh sedir-install

$(AUDIT_BIN): sedir-audit/Cargo.toml sedir-audit/Cargo.lock $(shell find sedir-audit/src -name '*.rs') scripts/build_installer.sh
	scripts/build_installer.sh sedir-audit
$(AUDIT_DEB): $(PKG_INPUTS) $(AUDIT_BIN)
	scripts/build_packages.sh sedir-audit

ifneq ($(PROFILE),install)
# Same recipe as $(ROOTFS_TAR), install profile.
$(INSTALL_ROOTFS_TAR): $(ROOTFS_INPUTS) $(INSTALL_DEBS)
	$(SUDO) env SEDIR_PROFILE=install scripts/build_rootfs.sh --clean
endif

$(ISO): $(INSTALL_ROOTFS_TAR) scripts/build_iso.sh $(wildcard config/iso/*)
	$(SUDO) scripts/build_iso.sh --rootfs $(INSTALL_ROOTFS_TAR)

$(CLI): $(CLI_INPUTS)
	cargo build --release --manifest-path sedir-cli/Cargo.toml

deb: $(DEBS) ## Build SedirOS .deb packages (build/debs)
rootfs: $(ROOTFS_TAR) ## Build the hardened rootfs archive (only if inputs changed)
image: $(IMAGE) ## Build the qcow2 base image
build: $(IMAGE) $(CLI) ## rootfs + image + sedir binary
iso: $(ISO) ## Build the live/installer ISO (install profile)

cli: $(CLI) ## Build sedir-cli (release)

installer: $(INSTALLER_BIN) ## Build sedir-install for Linux (Docker on macOS)
audit: $(AUDIT_BIN) ## Build sedir-audit for Linux (Docker on macOS)

install: $(CLI) ## Install 'sedir' into ~/.cargo/bin
	cargo install --path sedir-cli --locked --force

# --- quality -----------------------------------------------------------------

test: $(CLI) $(IMAGE) ## Boot a throwaway VM and run the hardening smoke test (vm profile)
	@test "$(PROFILE)" = vm || { echo "make test needs PROFILE=vm (install images have no key and no passwordless sudo)"; exit 1; }
	SEDIR_BIN=$(CLI) SEDIR_IMAGE=$(IMAGE) tests/smoke.sh

apt-test: $(CLI) $(IMAGE) $(DEBS) ## Update a throwaway VM through a test apt repository (signature, pin, unattended-upgrades)
	@test "$(PROFILE)" = vm || { echo "make apt-test needs PROFILE=vm"; exit 1; }
	SEDIR_BIN=$(CLI) SEDIR_IMAGE=$(IMAGE) tests/apt_repo.sh

lint: ## bash -n + shellcheck (if installed) + rustfmt + clippy + package versions
	@scripts/check_versions.sh
	@for f in $(SHELL_SCRIPTS); do bash -n "$$f" || exit 1; done; echo "bash -n: ok"
	@if command -v shellcheck >/dev/null; then shellcheck -x $(SHELL_SCRIPTS) && echo "shellcheck: ok"; \
	 else echo "shellcheck: not installed, skipped"; fi
	@for c in sedir-cli sedir-install sedir-audit; do \
	    cargo fmt --manifest-path $$c/Cargo.toml --check && \
	    cargo clippy --release --manifest-path $$c/Cargo.toml -- -D warnings || exit 1; \
	done

fmt: ## Format Rust code
	cargo fmt --manifest-path sedir-cli/Cargo.toml
	cargo fmt --manifest-path sedir-install/Cargo.toml
	cargo fmt --manifest-path sedir-audit/Cargo.toml

# --- VM shortcuts ------------------------------------------------------------

launch: $(CLI) $(IMAGE) ## Start VM (VM=dev)
	$(CLI) launch $(VM)

shell: $(CLI) ## SSH into VM (VM=dev)
	$(CLI) shell $(VM)

stop: $(CLI) ## Stop VM (VM=dev)
	$(CLI) stop $(VM)

iso-run: $(ISO) ## Boot the live ISO in QEMU (UEFI, serial console)
	scripts/run_iso.sh $(ISO)

iso-test: ## Boot the ISO with Secure Boot enforced and check the live session
	tests/iso_secureboot.sh $(ISO)

install-test: ## Install from the ISO and boot the installed disk, with Secure Boot
	tests/install_secureboot.sh $(ISO)

clean: ## Remove build/ artifacts and Rust target/ (VMs are kept)
	$(SUDO) rm -rf --one-file-system $(BUILD) 2>/dev/null || $(SUDO) rm -rf $(BUILD)
	cargo clean --manifest-path sedir-cli/Cargo.toml
	cargo clean --manifest-path sedir-install/Cargo.toml
	cargo clean --manifest-path sedir-audit/Cargo.toml
