
VERIFIED_BOOT_SIGN=INSTALLER='$(INSTALLER)' FILESYSTEM_CONFIG='$(FILESYSTEM_CONFIG)' VERIFIED_BOOT_SIGNER='$(VERIFIED_BOOT_SIGNER)' scripts/verified-boot-sign.sh $(REDOXFS) $(VERIFIED_BOOT_KEYS)

$(BUILD)/harddrive.img: $(FSTOOLS) $(REPO_TAG)
ifeq ($(FSTOOLS_IN_PODMAN),1)
	$(PODMAN_RUN) make $@
else
ifeq ($(VERIFIED_BOOT)$(FSTOOLS_NO_MOUNT),11)
	@echo "VERIFIED_BOOT=1 signs the image through a FUSE mount, which FSTOOLS_NO_MOUNT=1 leaves out; build with VERIFIED_BOOT=0 for an image whose bootloader verifies nothing" >&2 && exit 1
endif
	mkdir -p $(BUILD)
	FUMOUNT='$(FUMOUNT)' scripts/image-mount.sh unmount-if-mounted "$(MOUNT_DIR)"
	-$(FUMOUNT) /tmp/redox_installer || true
	rm -rf $@  $@.partial $(MOUNT_DIR)
	FILESYSTEM_SIZE=$(FILESYSTEM_SIZE) && \
	if [ -z "$$FILESYSTEM_SIZE" ] ; then \
	FILESYSTEM_SIZE=$(shell $(INSTALLER) --filesystem-size -c $(FILESYSTEM_CONFIG)); \
	fi && \
	truncate -s "$$FILESYSTEM_SIZE"m $@.partial
	umask 022 && $(INSTALLER) $(INSTALLER_OPTS) -c $(FILESYSTEM_CONFIG) $@.partial
ifeq ($(VERIFIED_BOOT),1)
	$(VERIFIED_BOOT_SIGN) $@.partial
endif
	mv $@.partial $@
endif

# Signs harddrive.img for verified boot again, after a push changed its kernel or initfs.
# p.<recipe> and push run it themselves when they unmounted the image
verified-boot-sign: $(FSTOOLS) FORCE
ifeq ($(FSTOOLS_IN_PODMAN),1)
	$(PODMAN_RUN) make $@
else
	scripts/image-mount.sh require-unmounted "$(MOUNT_DIR)" "verified-boot-sign"
	$(VERIFIED_BOOT_SIGN) $(BUILD)/harddrive.img
endif

$(BUILD)/redox-live.iso: $(FSTOOLS) $(REPO_TAG) redox.ipxe
ifeq ($(FSTOOLS_IN_PODMAN),1)
	$(PODMAN_RUN) make $@
else
	mkdir -p $(BUILD)
	rm -rf $@  $@.partial
	-$(FUMOUNT) /tmp/redox_installer || true
	FILESYSTEM_SIZE=$(FILESYSTEM_SIZE) && \
	if [ -z "$$FILESYSTEM_SIZE" ] ; then \
	FILESYSTEM_SIZE=$(shell $(INSTALLER) --filesystem-size -c $(FILESYSTEM_CONFIG)); \
	fi && \
	truncate -s "$$FILESYSTEM_SIZE"m $@.partial
	umask 022 && $(INSTALLER) $(INSTALLER_OPTS) -c $(FILESYSTEM_CONFIG) --write-bootloader="$(BUILD)/bootloader-live.efi" --live $@.partial
	mv $@.partial $@
	cp redox.ipxe $(BUILD)/redox.ipxe
endif

$(BUILD)/filesystem.img: $(FSTOOLS) $(REPO_TAG)
ifeq ($(FSTOOLS_IN_PODMAN),1)
	$(PODMAN_RUN) make $@
else
	mkdir -p $(BUILD)
	FUMOUNT='$(FUMOUNT)' scripts/image-mount.sh unmount-if-mounted "$(MOUNT_DIR)"
	rm -rf $@  $@.partial $(MOUNT_DIR)
	-$(FUMOUNT) /tmp/redox_installer || true
	FILESYSTEM_SIZE=$(FILESYSTEM_SIZE) && \
	if [ -z "$$FILESYSTEM_SIZE" ] ; then \
	FILESYSTEM_SIZE=$(shell $(INSTALLER) --filesystem-size -c $(FILESYSTEM_CONFIG)); \
	fi && \
	truncate -s "$$FILESYSTEM_SIZE"m $@.partial
	$(REDOXFS_MKFS) $(REDOXFS_MKFS_FLAGS) $@.partial
	FUMOUNT='$(FUMOUNT)' scripts/image-mount.sh mount "$(REDOXFS)" "$@.partial" "$(MOUNT_DIR)"
	umask 022 && $(INSTALLER) $(INSTALLER_OPTS) -c $(FILESYSTEM_CONFIG) $(MOUNT_DIR)
	FUMOUNT='$(FUMOUNT)' scripts/image-mount.sh unmount "$(MOUNT_DIR)"
	mv $@.partial $@
endif

mount: $(FSTOOLS) FORCE
ifeq ($(FSTOOLS_IN_PODMAN),1)
	$(PODMAN_RUN) make $@
else
	FUMOUNT='$(FUMOUNT)' scripts/image-mount.sh mount "$(REDOXFS)" "$(BUILD)/harddrive.img" "$(MOUNT_DIR)"
endif

mount_extra: $(FSTOOLS) FORCE
ifeq ($(FSTOOLS_IN_PODMAN),1)
	$(PODMAN_RUN) make $@
else
	FUMOUNT='$(FUMOUNT)' scripts/image-mount.sh mount "$(REDOXFS)" "$(BUILD)/extra.img" "$(MOUNT_DIR)"
endif

mount_live: $(FSTOOLS) FORCE
ifeq ($(FSTOOLS_IN_PODMAN),1)
	$(PODMAN_RUN) make $@
else
	FUMOUNT='$(FUMOUNT)' scripts/image-mount.sh mount "$(REDOXFS)" "$(BUILD)/redox-live.iso" "$(MOUNT_DIR)"
endif

unmount: FORCE
ifeq ($(FSTOOLS_IN_PODMAN),1)
	$(PODMAN_RUN) make $@
else
	FUMOUNT='$(FUMOUNT)' scripts/image-mount.sh unmount "$(MOUNT_DIR)"
	@-$(FUMOUNT) /tmp/redox_installer 2>/dev/null || true
endif
