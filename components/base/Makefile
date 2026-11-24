TARGET ?= x86_64-unknown-redox
LINKER ?= $(shell redoxer env which $(shell redoxer env printenv LD))
BUILD_TYPE ?= release
BUILD_FLAGS ?= --release
CARGO ?= redoxer
CARGO_HOST ?= env -u CARGO -u RUSTFLAGS cargo

SRC_DIR ?= $(CURDIR)
BUILD_DIR ?= $(shell pwd)/target/$(TARGET)/build
DESTDIR ?= ./sysroot
SYSROOT ?= $(shell pwd)/target/$(TARGET)/sysroot
TARGET_DIR = $(BUILD_DIR)/$(TARGET)/$(BUILD_TYPE)
BUILD_FLAGS +=  --target-dir $(BUILD_DIR)

INITFS_BINS = ipcd init logd ramfs randd udsd zerod inputd
BASE_BINS = getty inputd audiod dhcpd ptyd netstack
SELFTEST_DIRS := $(patsubst selftests/%/Cargo.toml,%,$(wildcard selftests/*/Cargo.toml))
TEST_BINS := $(addsuffix -selftest,$(SELFTEST_DIRS))

INITFS_STAGE = $(DESTDIR)/usr/lib/initfs

INITFS_CARGO_ARGS = $(foreach bin,$(INITFS_BINS),-p $(bin))
BASE_CARGO_ARGS = $(foreach bin,$(BASE_BINS),-p $(bin))
TEST_CARGO_ARGS = $(foreach bin,$(TEST_BINS),-p $(bin))

.PHONY: all base install install-base install-initfs test test-gui test-ipcd selftests test-selftests test-virtio-fs

all: base
install: install-base

clean:
	rm -rf $(SRC_DIR)/target $(SRC_DIR)/sysroot $(SYSROOT) $(TARGET_DIR)

# test if booting
test: all
	$(MAKE) install
	REDOXER_SYSROOT=$(DESTDIR) redoxer exec true

test-selftests: all
	$(MAKE) install
	$(MAKE) selftests
	@for bin in $(TEST_BINS); do \
		cp -v "$(TARGET_DIR)/$$bin" "$(DESTDIR)/usr/bin"; \
	done
	@for d in $(SELFTEST_DIRS); do \
		run="$(SRC_DIR)/selftests/$$d/scripts/redoxer-run.sh"; \
		if [ -x "$$run" ]; then \
			REDOXER_SYSROOT=$(DESTDIR) "$$run"; \
		else \
			env -u REDOXER_SYSROOT redoxer exec --folder "$(DESTDIR)/:/" $$d-selftest; \
		fi; \
	done

test-virtio-fs: all
	$(MAKE) install
	$(MAKE) selftests
	cp -v "$(TARGET_DIR)/virtio-fsd-selftest" "$(DESTDIR)/usr/bin"
	REDOXER_SYSROOT=$(DESTDIR) $(SRC_DIR)/selftests/virtio-fsd/scripts/redoxer-run.sh

# test with interactive gui
test-gui: all
	$(MAKE) install
	REDOXER_SYSROOT=$(DESTDIR) redoxer exec --gui ion

# test the ipcd examples
test-ipcd: all
	$(MAKE) install
	$(CARGO) build --target $(TARGET) -p ipcd --examples --target-dir $(BUILD_DIR)/ipcd
	REDOXER_SYSROOT=$(DESTDIR) redoxer exec --folder $(SRC_DIR)/ipcd:/root --folder $(BUILD_DIR)/ipcd/$(TARGET)/debug:/root/target -- sh -- ipcd/redoxer.sh event main unnamed shm_demo
	# FIXME: shm is currently broken

clippy:
	redoxer clippy
	cd bootstrap && redoxer clippy

# -----------------------------------------------------------------------------
# base
# -----------------------------------------------------------------------------
$(SYSROOT)/bin/redoxfs:
	REDOXER_SYSROOT=$(SYSROOT) redoxer pkg redoxfs

base:
	@mkdir -pv "$(BUILD_DIR)"
	CARGO_PROFILE_RELEASE_OPT_LEVEL=s CARGO_PROFILE_RELEASE_PANIC=abort \
		$(CARGO) build $(BUILD_FLAGS) \
		--manifest-path "$(SRC_DIR)/Cargo.toml" \
		$(BASE_CARGO_ARGS)
# FIXME fix whatever issue (feature unification?) causes most logs to be omitted
# if this is merged with the above build command.
	CARGO_PROFILE_RELEASE_OPT_LEVEL=s CARGO_PROFILE_RELEASE_PANIC=abort \
		$(CARGO) build $(BUILD_FLAGS) \
		--manifest-path "$(SRC_DIR)/Cargo.toml" \
		$(INITFS_CARGO_ARGS)
# Build bootstrap
	cd "$(SRC_DIR)/bootstrap" && RUSTFLAGS= $(CARGO) rustc $(BUILD_FLAGS) \
		-- -Ctarget-feature=+crt-static -Clinker="$(LINKER)"

# Not part of the image. CI runs these under redoxer. the virtio-fsd-selftest
# package is the opt-in build for a live system.
selftests:
	@mkdir -pv "$(BUILD_DIR)"
		$(CARGO) build $(BUILD_FLAGS) \
		--manifest-path "$(SRC_DIR)/Cargo.toml" \
		$(TEST_CARGO_ARGS)

install-base: base
	@mkdir -pv "$(DESTDIR)/usr/bin"
# Distribute binaries
	@for bin in $(BASE_BINS); do \
		cp -v "$(TARGET_DIR)/$$bin" "$(DESTDIR)/usr/bin"; \
	done
# Copy configurations
	@mkdir -pv "$(DESTDIR)/usr/lib/init.d/"
	@cp -v "$(SRC_DIR)/init.d"/* "$(DESTDIR)/usr/lib/init.d/"

	@mkdir -pv "$(INITFS_STAGE)/root/bin" "$(INITFS_STAGE)/root/lib/init.d"
	for bin in $(INITFS_BINS); do \
		cp -v "$(TARGET_DIR)/$$bin" "$(INITFS_STAGE)/root/bin"; \
	done
	cp "$(SRC_DIR)/init.initfs.d"/* "$(INITFS_STAGE)/root/lib/init.d/"
	cp -v "$(TARGET_DIR)/bootstrap" "$(INITFS_STAGE)/bootstrap"

# Device file symlinks
	@mkdir -pv "$(DESTDIR)/dev"
	ln -sf /scheme/null $(DESTDIR)/dev/null
	ln -sf /scheme/pty/ptmx $(DESTDIR)/dev/ptmx
	ln -sf /scheme/rand $(DESTDIR)/dev/random
	ln -sf /scheme/rand $(DESTDIR)/dev/urandom
	ln -sf /scheme/zero $(DESTDIR)/dev/zero
	ln -sf /scheme/libc/tty $(DESTDIR)/dev/tty
	ln -sf /scheme/libc/stdin $(DESTDIR)/dev/stdin
	ln -sf /scheme/libc/stdout $(DESTDIR)/dev/stdout
	ln -sf /scheme/libc/stderr $(DESTDIR)/dev/stderr

install-initfs: $(SYSROOT)/bin/redoxfs
	rm -rf "$(BUILD_DIR)/initfs"
	@mkdir -pv "$(BUILD_DIR)/initfs"
	cp -r "$(SYSROOT)/usr/lib/initfs/root/." "$(BUILD_DIR)/initfs/"
	cp "$(SYSROOT)/bin/redoxfs" "$(BUILD_DIR)/initfs/bin"
	$(CARGO_HOST) run --manifest-path "$(SRC_DIR)/initfs/tools/Cargo.toml" --bin redox-initfs-ar -- \
		"$(BUILD_DIR)/initfs" "$(SYSROOT)/usr/lib/initfs/bootstrap" -o "$(BUILD_DIR)/initfs.img"
	@mkdir -pv "$(DESTDIR)/usr/lib/boot"
	cp -v "$(BUILD_DIR)/initfs.img" "$(DESTDIR)/usr/lib/boot/initfs"
