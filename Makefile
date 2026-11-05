# =============================================================================
#  Sisura - developer workflow
#  Works with GNU make >= 3.81 (macOS /usr/bin/make) on Linux and macOS.
#  On macOS the build scripts re-run themselves inside Docker.
# =============================================================================
SHELL := /bin/bash
.DEFAULT_GOAL := help

include config/sisura.conf

UNAME_S := $(shell uname -s)
UNAME_M := $(shell uname -m)
ARCH := $(if $(filter x86_64,$(UNAME_M)),amd64,$(if $(filter arm64 aarch64,$(UNAME_M)),arm64,unsupported))

BUILD      := build
PROFILE_TAG := $(if $(filter vm,$(PROFILE)),,-$(PROFILE))
ROOTFS_TAR := $(BUILD)/sisura-rootfs-$(RELEASE)$(PROFILE_TAG)-$(ARCH).tar.zst
IMAGE      := $(BUILD)/sisura-$(RELEASE)$(PROFILE_TAG)-$(ARCH).qcow2
# The ISO (live session + installer) is always built from the install profile.
INSTALL_ROOTFS_TAR := $(BUILD)/sisura-rootfs-$(RELEASE)-install-$(ARCH).tar.zst
ISO        := $(BUILD)/sisura-$(RELEASE)-install-$(ARCH).iso
CLI        := sisura-cli/target/release/sisura
SSH_KEY    := id_sisura
pkg_version = $(shell sed -n 's/^Version: *//p' packages/$(1)/DEBIAN/control)
DEBS       := $(BUILD)/debs/sisura-base-files_$(RELEASE)_all.deb \
              $(BUILD)/debs/sisura-hardening_$(call pkg_version,sisura-hardening)_all.deb \
              $(BUILD)/debs/sisura-boot-hardening_$(call pkg_version,sisura-boot-hardening)_all.deb
# Only in the install profile (live ISO + installer).
INSTALLER_BIN := $(BUILD)/bin/sisura-install-$(ARCH)
LIVE_DEB      := $(BUILD)/debs/sisura-live_$(call pkg_version,sisura-live)_all.deb
INSTALLER_DEB := $(BUILD)/debs/sisura-install_$(call pkg_version,sisura-install)_$(ARCH).deb
INSTALL_DEBS  := $(LIVE_DEB) $(INSTALLER_DEB)

# Linux builds need root (debootstrap/chroot, file ownership); -E keeps SISURA_* env.
SUDO := $(if $(filter Linux,$(UNAME_S)),$(if $(filter-out 0,$(shell id -u)),sudo -E,),)

export SISURA_RELEASE       := $(RELEASE)
# SUITE is derived from RELEASE by the scripts; set it only for sid builds.
ifdef SUITE
export SISURA_SUITE         := $(SUITE)
endif
export SISURA_USER          := $(GUEST_USER)
export SISURA_PROFILE       := $(PROFILE)
# The dev key only goes into vm images; install images get keys at first boot.
ifeq ($(PROFILE),vm)
export SISURA_SSH_PUBKEY    := $(SSH_KEY).pub
endif
export SISURA_MAX_BOOT_SECS := $(MAX_BOOT_SECS)
export SISURA_MAX_RAM_MB    := $(MAX_RAM_MB)

# Inputs that change the rootfs (config/sisura.conf only holds make variables).
ROOTFS_INPUTS := scripts/build_rootfs.sh scripts/lib/release.sh $(DEBS) $(if $(filter install,$(PROFILE)),$(INSTALL_DEBS)) $(filter-out config/sisura.conf config/iso/%,$(shell find config -type f)) $(SSH_KEY).pub
PKG_INPUTS    := scripts/build_packages.sh scripts/lib/release.sh config/sisura.conf $(shell find packages -type f ! -name .DS_Store)
CLI_INPUTS    := sisura-cli/Cargo.toml $(shell find sisura-cli/src -name '*.rs')
SHELL_SCRIPTS := $(wildcard scripts/*.sh scripts/lib/*.sh tests/*.sh)

.PHONY: help key deb rootfs image build iso iso-run cli installer install test lint fmt launch shell stop clean

help: ## Show this help
	@echo "Sisura $(RELEASE) ($(ARCH), profile $(PROFILE)) - usage: make <target> [RELEASE=..] [PROFILE=vm|install] [VM=..]"
	@echo
	@grep -hE '^[a-z-]+:.*## ' $(MAKEFILE_LIST) | awk -F':.*## ' '{printf "  \033[1m%-10s\033[0m %s\n", $$1, $$2}'

# --- artifacts ---------------------------------------------------------------

$(SSH_KEY).pub:
	@test -f $(SSH_KEY) || ssh-keygen -q -t ed25519 -N "" -C sisura-dev -f $(SSH_KEY)
	@test -f $@ || ssh-keygen -y -f $(SSH_KEY) > $@
	@echo "SSH key: $(SSH_KEY)"

# One build_packages.sh run produces every .deb (grouped target emulation for make 3.81).
$(firstword $(DEBS)): $(PKG_INPUTS)
	scripts/build_packages.sh
$(wordlist 2,$(words $(DEBS)),$(DEBS)): $(firstword $(DEBS))
	@test -f $@ || { echo "$@ missing after build_packages.sh"; exit 1; }

$(ROOTFS_TAR): $(ROOTFS_INPUTS)
	$(SUDO) scripts/build_rootfs.sh --clean

$(IMAGE): $(ROOTFS_TAR) scripts/build_image.sh
	$(SUDO) scripts/build_image.sh --rootfs $(ROOTFS_TAR) --size $(IMAGE_SIZE)

$(INSTALLER_BIN): sisura-install/Cargo.toml sisura-install/Cargo.lock $(shell find sisura-install/src -name '*.rs') scripts/build_installer.sh
	scripts/build_installer.sh

# sisura-live is built with the other arch-independent packages; the
# installer package needs its binary first.
$(LIVE_DEB): $(firstword $(DEBS))
	@test -f $@ || { echo "$@ missing after build_packages.sh"; exit 1; }
$(INSTALLER_DEB): $(PKG_INPUTS) $(INSTALLER_BIN)
	scripts/build_packages.sh

ifneq ($(PROFILE),install)
# Same recipe as $(ROOTFS_TAR), without the dev key.
$(INSTALL_ROOTFS_TAR): $(ROOTFS_INPUTS) $(INSTALL_DEBS)
	$(SUDO) env -u SISURA_SSH_PUBKEY SISURA_PROFILE=install scripts/build_rootfs.sh --clean
endif

$(ISO): $(INSTALL_ROOTFS_TAR) scripts/build_iso.sh $(wildcard config/iso/*)
	$(SUDO) scripts/build_iso.sh --rootfs $(INSTALL_ROOTFS_TAR)

$(CLI): $(CLI_INPUTS)
	cargo build --release --manifest-path sisura-cli/Cargo.toml

key: $(SSH_KEY).pub ## Generate the dev SSH key (id_sisura)
deb: $(DEBS) ## Build Sisura .deb packages (build/debs)
rootfs: $(ROOTFS_TAR) ## Build the hardened rootfs archive (only if inputs changed)
image: $(IMAGE) ## Build the qcow2 base image
build: $(IMAGE) $(CLI) ## rootfs + image + sisura binary
iso: $(ISO) ## Build the live/installer ISO (install profile)

cli: $(CLI) ## Build sisura-cli (release)

installer: $(INSTALLER_BIN) ## Build sisura-install for Linux (Docker on macOS)

install: $(CLI) ## Install 'sisura' into ~/.cargo/bin
	cargo install --path sisura-cli --locked --force

# --- quality -----------------------------------------------------------------

test: $(CLI) $(IMAGE) ## Boot a throwaway VM and run the hardening smoke test (vm profile)
	@test "$(PROFILE)" = vm || { echo "make test needs PROFILE=vm (install images have no key and no passwordless sudo)"; exit 1; }
	SISURA_BIN=$(CLI) SISURA_IMAGE=$(IMAGE) tests/smoke.sh

lint: ## bash -n + shellcheck (if installed) + rustfmt + clippy + package versions
	@scripts/check_versions.sh
	@for f in $(SHELL_SCRIPTS); do bash -n "$$f" || exit 1; done; echo "bash -n: ok"
	@if command -v shellcheck >/dev/null; then shellcheck -x $(SHELL_SCRIPTS) && echo "shellcheck: ok"; \
	 else echo "shellcheck: not installed, skipped"; fi
	@for c in sisura-cli sisura-install; do \
	    cargo fmt --manifest-path $$c/Cargo.toml --check && \
	    cargo clippy --release --manifest-path $$c/Cargo.toml -- -D warnings || exit 1; \
	done

fmt: ## Format Rust code
	cargo fmt --manifest-path sisura-cli/Cargo.toml
	cargo fmt --manifest-path sisura-install/Cargo.toml

# --- VM shortcuts ------------------------------------------------------------

launch: $(CLI) $(IMAGE) ## Start VM (VM=dev)
	$(CLI) launch $(VM)

shell: $(CLI) ## SSH into VM (VM=dev)
	$(CLI) shell $(VM)

stop: $(CLI) ## Stop VM (VM=dev)
	$(CLI) stop $(VM)

iso-run: $(ISO) ## Boot the live ISO in QEMU (UEFI, serial console)
	scripts/run_iso.sh $(ISO)

clean: ## Remove build/ artifacts and Rust target/ (VMs are kept)
	$(SUDO) rm -rf --one-file-system $(BUILD) 2>/dev/null || $(SUDO) rm -rf $(BUILD)
	cargo clean --manifest-path sisura-cli/Cargo.toml
	cargo clean --manifest-path sisura-install/Cargo.toml
