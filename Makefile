# =============================================================================
#  SedirOS - developer workflow
#  Works with GNU make >= 3.81 (macOS /usr/bin/make) on Linux and macOS.
#  On macOS the build scripts re-run themselves inside Docker.
# =============================================================================
SHELL := /bin/bash
.DEFAULT_GOAL := help

include config/sediros.conf

UNAME_S := $(shell uname -s)
UNAME_M := $(shell uname -m)
ARCH := $(if $(filter x86_64,$(UNAME_M)),amd64,$(if $(filter arm64 aarch64,$(UNAME_M)),arm64,unsupported))

BUILD      := build
PROFILE_TAG := $(if $(filter vm,$(PROFILE)),,-$(PROFILE))
ROOTFS_TAR := $(BUILD)/sediros-rootfs-$(RELEASE)$(PROFILE_TAG)-$(ARCH).tar.zst
IMAGE      := $(BUILD)/sediros-$(RELEASE)$(PROFILE_TAG)-$(ARCH).qcow2
# The ISO (live session + installer) is always built from the install profile.
INSTALL_ROOTFS_TAR := $(BUILD)/sediros-rootfs-$(RELEASE)-install-$(ARCH).tar.zst
ISO        := $(BUILD)/sediros-$(RELEASE)-install-$(ARCH).iso
CLI        := sediros-cli/target/release/sediros
pkg_version = $(shell sed -n 's/^Version: *//p' packages/$(1)/DEBIAN/control)
DEBS       := $(BUILD)/debs/sediros-base-files_$(RELEASE)_all.deb \
              $(BUILD)/debs/sediros-hardening_$(call pkg_version,sediros-hardening)_all.deb \
              $(BUILD)/debs/sediros-boot-hardening_$(call pkg_version,sediros-boot-hardening)_all.deb \
              $(BUILD)/debs/sediros-guest_$(call pkg_version,sediros-guest)_all.deb \
              $(BUILD)/debs/sediros-archive-keyring_$(call pkg_version,sediros-archive-keyring)_all.deb
# Only in the install profile (live ISO + installer).
INSTALLER_BIN := $(BUILD)/bin/sediros-install-$(ARCH)
LIVE_DEB      := $(BUILD)/debs/sediros-live_$(call pkg_version,sediros-live)_all.deb
INSTALLER_DEB := $(BUILD)/debs/sediros-install_$(call pkg_version,sediros-install)_$(ARCH).deb
INSTALL_DEBS  := $(LIVE_DEB) $(INSTALLER_DEB)
# In every image (vm and install profile).
AUDIT_BIN     := $(BUILD)/bin/sediros-audit-$(ARCH)
AUDIT_DEB     := $(BUILD)/debs/sediros-audit_$(call pkg_version,sediros-audit)_$(ARCH).deb
# Built and published (apt repository), but in no image yet: it is for cloud images.
FIRSTBOOT_BIN := $(BUILD)/bin/sediros-firstboot-$(ARCH)
FIRSTBOOT_DEB := $(BUILD)/debs/sediros-firstboot_$(call pkg_version,sediros-firstboot)_$(ARCH).deb

# Linux builds need root (debootstrap/chroot, file ownership); -E keeps SEDIROS_* env.
SUDO := $(if $(filter Linux,$(UNAME_S)),$(if $(filter-out 0,$(shell id -u)),sudo -E,),)

export SEDIROS_RELEASE       := $(RELEASE)
# SUITE is derived from RELEASE by the scripts; set it only for sid builds.
ifdef SUITE
export SEDIROS_SUITE         := $(SUITE)
endif
export SEDIROS_USER          := $(GUEST_USER)
export SEDIROS_PROFILE       := $(PROFILE)
# No SSH key is baked into any image: vm images get the sediros CLI's key at
# boot (systemd credential), installed systems get keys from the installer.
export SEDIROS_MAX_BOOT_SECS := $(MAX_BOOT_SECS)
export SEDIROS_MAX_RAM_MB    := $(MAX_RAM_MB)

# Inputs that change the rootfs (config/sediros.conf only holds make variables).
ROOTFS_INPUTS := scripts/build_rootfs.sh scripts/lib/release.sh $(DEBS) $(AUDIT_DEB) $(if $(filter install,$(PROFILE)),$(INSTALL_DEBS)) $(filter-out config/sediros.conf config/iso/%,$(shell find config -type f))
PKG_INPUTS    := scripts/build_packages.sh scripts/lib/release.sh config/sediros.conf $(shell find packages -type f ! -name .DS_Store)
CLI_INPUTS    := sediros-cli/Cargo.toml $(shell find sediros-cli/src -name '*.rs')
SHELL_SCRIPTS := $(wildcard scripts/*.sh scripts/lib/*.sh tests/*.sh tests/lib/*.sh)

.PHONY: help deb rootfs image build iso iso-run iso-test install-test apt-test firstboot-test cli installer audit firstboot install test lint fmt launch shell stop clean

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

$(INSTALLER_BIN): sediros-install/Cargo.toml sediros-install/Cargo.lock $(shell find sediros-install/src -name '*.rs') scripts/build_installer.sh
	scripts/build_installer.sh

# sediros-live is built with the other arch-independent packages; the
# installer package needs its binary first.
$(LIVE_DEB): $(firstword $(DEBS))
	@test -f $@ || { echo "$@ missing after build_packages.sh"; exit 1; }
$(INSTALLER_DEB): $(PKG_INPUTS) $(INSTALLER_BIN)
	scripts/build_packages.sh sediros-install

$(AUDIT_BIN): sediros-audit/Cargo.toml sediros-audit/Cargo.lock $(shell find sediros-audit/src -name '*.rs') scripts/build_installer.sh
	scripts/build_installer.sh sediros-audit
$(AUDIT_DEB): $(PKG_INPUTS) $(AUDIT_BIN)
	scripts/build_packages.sh sediros-audit

$(FIRSTBOOT_BIN): sediros-firstboot/Cargo.toml sediros-firstboot/Cargo.lock $(shell find sediros-firstboot/src -name '*.rs') scripts/build_installer.sh
	scripts/build_installer.sh sediros-firstboot
$(FIRSTBOOT_DEB): $(PKG_INPUTS) $(FIRSTBOOT_BIN)
	scripts/build_packages.sh sediros-firstboot

ifneq ($(PROFILE),install)
# Same recipe as $(ROOTFS_TAR), install profile.
$(INSTALL_ROOTFS_TAR): $(ROOTFS_INPUTS) $(INSTALL_DEBS)
	$(SUDO) env SEDIROS_PROFILE=install scripts/build_rootfs.sh --clean
endif

$(ISO): $(INSTALL_ROOTFS_TAR) scripts/build_iso.sh $(wildcard config/iso/*)
	$(SUDO) scripts/build_iso.sh --rootfs $(INSTALL_ROOTFS_TAR)

$(CLI): $(CLI_INPUTS)
	cargo build --release --manifest-path sediros-cli/Cargo.toml

deb: $(DEBS) ## Build SedirOS .deb packages (build/debs)
rootfs: $(ROOTFS_TAR) ## Build the hardened rootfs archive (only if inputs changed)
image: $(IMAGE) ## Build the qcow2 base image
build: $(IMAGE) $(CLI) $(FIRSTBOOT_DEB) ## rootfs + image + sediros binary + packages outside the image
iso: $(ISO) ## Build the live/installer ISO (install profile)

cli: $(CLI) ## Build sediros-cli (release)

installer: $(INSTALLER_BIN) ## Build sediros-install for Linux (Docker on macOS)
audit: $(AUDIT_BIN) ## Build sediros-audit for Linux (Docker on macOS)
firstboot: $(FIRSTBOOT_DEB) ## Build the sediros-firstboot package (Docker on macOS)

install: $(CLI) ## Install 'sediros' into ~/.cargo/bin
	cargo install --path sediros-cli --locked --force

# --- quality -----------------------------------------------------------------

test: $(CLI) $(IMAGE) ## Boot a throwaway VM and run the hardening smoke test (vm profile)
	@test "$(PROFILE)" = vm || { echo "make test needs PROFILE=vm (install images have no key and no passwordless sudo)"; exit 1; }
	SEDIROS_BIN=$(CLI) SEDIROS_IMAGE=$(IMAGE) tests/smoke.sh

firstboot-test: $(CLI) $(IMAGE) $(FIRSTBOOT_DEB) ## Install sediros-firstboot in a throwaway VM and set it up from a fake metadata service
	@test "$(PROFILE)" = vm || { echo "make firstboot-test needs PROFILE=vm"; exit 1; }
	SEDIROS_BIN=$(CLI) SEDIROS_IMAGE=$(IMAGE) tests/firstboot.sh $(FIRSTBOOT_DEB)

apt-test: $(CLI) $(IMAGE) $(DEBS) ## Update a throwaway VM through a test apt repository (signature, pin, unattended-upgrades)
	@test "$(PROFILE)" = vm || { echo "make apt-test needs PROFILE=vm"; exit 1; }
	SEDIROS_BIN=$(CLI) SEDIROS_IMAGE=$(IMAGE) tests/apt_repo.sh

lint: ## bash -n + shellcheck (if installed) + rustfmt + clippy + package versions
	@scripts/check_versions.sh
	@for f in $(SHELL_SCRIPTS); do bash -n "$$f" || exit 1; done; echo "bash -n: ok"
	@if command -v shellcheck >/dev/null; then shellcheck -x $(SHELL_SCRIPTS) && echo "shellcheck: ok"; \
	 else echo "shellcheck: not installed, skipped"; fi
	@for c in sediros-cli sediros-install sediros-audit sediros-firstboot; do \
	    cargo fmt --manifest-path $$c/Cargo.toml --check && \
	    cargo clippy --release --manifest-path $$c/Cargo.toml -- -D warnings || exit 1; \
	done

fmt: ## Format Rust code
	cargo fmt --manifest-path sediros-cli/Cargo.toml
	cargo fmt --manifest-path sediros-install/Cargo.toml
	cargo fmt --manifest-path sediros-audit/Cargo.toml
	cargo fmt --manifest-path sediros-firstboot/Cargo.toml

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
	cargo clean --manifest-path sediros-cli/Cargo.toml
	cargo clean --manifest-path sediros-install/Cargo.toml
	cargo clean --manifest-path sediros-audit/Cargo.toml
	cargo clean --manifest-path sediros-firstboot/Cargo.toml
