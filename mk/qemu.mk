
QEMU?=qemu-system-$(QEMU_ARCH)
QEMUFLAGS?=-d guest_errors -name "FerrionOS $(ARCH)"
## gpu=venus opts into Venus; the default display device remains unchanged.
## Host-visible GPU memory aperture for Venus (independent of guest RAM).
QEMU_GPU_HOSTMEM?=4G
## Display backend: gtk,gl=on for 3D, SDL for virtio-sdl, otherwise QEMU chooses; tests use egl-headless or none.
QEMU_DISPLAY?=$(if $(filter venus virtio-gl,$(gpu)),gtk$(comma)gl=on,$(if $(filter virtio-sdl,$(gpu)),sdl$(comma)show-cursor=on))
## EGL headless render node, such as /dev/dri/renderD128; empty lets QEMU choose.
QEMU_RENDER_NODE?=
## Options appended to the virtio display device of gpu=virtio and gpu=multi, such as ,xres=1920,yres=1080.
QEMU_GPU_OPTS?=
## Vulkan ICD selection for Venus; defaults to the host's VK_DRIVER_FILES.
QEMU_VK_DRIVER_FILES?=$(VK_DRIVER_FILES)
netboot?=no
redoxer?=no
VGA_SUPPORTED=no
QEMU_PATH?=$(shell command -v $(QEMU))
QEMU_BASE_DIR?=$(shell if [ $(QEMU_PATH) ]; then realpath $$(dirname $(QEMU_PATH))/..; fi)

ifeq ($(ARCH),x86_64)
	gpu?=vga
	uefi?=yes
	VGA_SUPPORTED=yes
	QEMU_ARCH=x86_64
	QEMU_MACHINE?=q35
	QEMU_CPU?=core2duo
	QEMU_SMP?=4
	QEMU_MEM?=2048
	ifeq ($(uefi),yes)
		FIRMWARE=$(firstword \
			$(wildcard /usr/share/ovmf/OVMF.fd) \
			$(wildcard /usr/share/OVMF/OVMF_CODE.fd) \
			$(wildcard /usr/share/edk2/x64/OVMF.4m.fd) \
		)
		ifeq ($(FIRMWARE),)
			PFLASH0=$(firstword \
				$(wildcard /usr/share/qemu/edk2-x86_64-code.fd) \
				$(wildcard $(QEMU_BASE_DIR)/share/qemu/edk2-x86_64-code.fd) \
			)
		endif
	endif
	ifneq ($(usb),no)
		QEMUFLAGS+=-device qemu-xhci
	endif
else ifeq ($(ARCH),aarch64)
	uefi?=yes
	live?=yes
	gpu?=ramfb
	QEMU_ARCH=aarch64
	QEMU_MACHINE?=virt
	QEMU_CPU=max
	QEMU_SMP?=1
	QEMU_MEM?=2048
	ifeq ($(BOARD),raspi3bp)
		QEMU_KERNEL=$(BUILD)/raspi3bp_uboot.rom
		disk?=sdcard
		gpu=none
		QEMU_MACHINE:=raspi3b
		QEMU_SMP:=4
		QEMU_MEM:=1024
		net:=usb-net
		audio:=no
		ifneq ($(usb),no)
			QEMUFLAGS+=-usb -device usb-kbd -device usb-tablet
		endif
	else
		ifeq ($(uefi),yes)
			FIRMWARE=$(firstword \
				$(wildcard /usr/share/AAVMF/AAVMF_CODE.fd) \
				$(wildcard /usr/share/edk2/aarch64/QEMU_EFI.fd) \
			)
			ifeq ($(FIRMWARE),)
				PFLASH0=$(firstword \
					$(wildcard /usr/share/qemu/edk2-aarch64-code.fd) \
					$(wildcard $(QEMU_BASE_DIR)/share/qemu/edk2-aarch64-code.fd) \
				)
				PFLASH0_NEEDED=yes
			endif
		else
			FIRMWARE=$(BUILD)/qemu_uboot.rom
		endif
		ifneq ($(usb),no)
			QEMUFLAGS+=-device qemu-xhci -device usb-kbd -device usb-tablet
		endif
	endif

	ifeq ($(HOST_ARCH),arm64)
		kvm?=yes
	endif
else ifeq ($(ARCH),riscv64gc)
	live=no
	audio=no
	gpu?=ramfb
	net=bridge
	QEMU_ARCH=riscv64
	# QEMU_MACHINE=virt  for ACPI mode instead of DTB
	QEMU_MACHINE=virt,acpi=off
	QEMU_SMP?=4
	QEMU_MEM?=2048
	QEMU_CPU=max
	PFLASH0=$(firstword \
		$(wildcard /usr/share/qemu-efi-riscv64/RISCV_VIRT_CODE.fd) \
		$(wildcard /usr/share/edk2/riscv/RISCV_VIRT_CODE.fd) \
		$(wildcard /usr/share/qemu/edk2-riscv-code.fd) \
		$(wildcard $(QEMU_BASE_DIR)/share/qemu/edk2-riscv-code.fd) \
	)
	PFLASH0_NEEDED=yes
	PFLASH1=$(firstword \
		$(wildcard /usr/share/qemu-efi-riscv64/RISCV_VIRT_VARS.fd) \
		$(wildcard /usr/share/edk2/riscv/RISCV_VIRT_VARS.fd) \
		$(wildcard /usr/share/qemu/edk2-riscv-vars.fd) \
		$(wildcard $(QEMU_BASE_DIR)/share/qemu/edk2-riscv-vars.fd) \
	)
	PFLASH1_NEEDED=yes
	ifneq ($(usb),no)
		QEMUFLAGS+=-device qemu-xhci -device usb-kbd -device usb-tablet
	endif
else
$(error Unsupported ARCH for QEMU "$(ARCH)"))
endif

QEMUFLAGS+=-smp $(QEMU_SMP) -m $(QEMU_MEM)

ifneq ($(ARCH),$(HOST_ARCH))
	kvm?=no
endif

ifeq ($(QEMU_ON_WINDOWS),1)
	QEMU:=$(QEMU).exe
	WINDOWS_DISK=/mnt/c/ProgramData/redox.img
	disk=windows
	net=windows
	QEMU_MACHINE=pc
	FIRMWARE=
	QEMU_KERNEL=
	QEMUFLAGS+=-device usb-tablet
endif

ifneq ($(FIRMWARE),)
	QEMUFLAGS+=-bios $(FIRMWARE)
endif

ifneq ($(QEMU_KERNEL),)
	QEMUFLAGS+=-kernel $(QEMU_KERNEL)
endif

ifeq ($(live),yes)
	DISK=$(BUILD)/redox-live.iso
else
	DISK=$(BUILD)/harddrive.img
endif

ifeq ($(serial),no)
	QEMUFLAGS+=-chardev stdio,id=debug -device isa-debugcon,iobase=0x402,chardev=debug
else
	QEMUFLAGS+=-chardev stdio,id=debug,signal=off,mux=on,"$(if $(qemu_serial_logfile),logfile=$(qemu_serial_logfile))"
	QEMUFLAGS+=-serial chardev:debug -mon chardev=debug
endif

ifeq ($(redoxer),yes)
ifeq ($(ARCH),x86_64)
	QEMUFLAGS+=-device isa-debug-exit
else ifeq ($(ARCH),aarch64)
	QEMUFLAGS+=-semihosting-config enable=on,target=native,userspace=on
endif
endif

# IOMMU (iommu=yes): attaches an emulated Intel IOMMU (VT-d) with interrupt remapping. QEMU has
# the device on the q35 machine only, so the option is for x86_64. Interrupt remapping does not
# work with KVM's in-kernel interrupt controller, so the controller is split between KVM and
# QEMU (kernel-irqchip=split). caching-mode=on makes the guest report every change of its
# translation tables to the device, which a device passed through from the host would need.
# iommu=test adds QEMU's test devices behind the same IOMMU: iommu-testdev, which writes a
# pattern by DMA where it is told, and edu, which raises an interrupt on request.
ifeq ($(iommu),yes)
	QEMUFLAGS+=-machine $(QEMU_MACHINE),kernel-irqchip=split -device intel-iommu,intremap=on,caching-mode=on
else ifeq ($(iommu),test)
	QEMUFLAGS+=-machine $(QEMU_MACHINE),kernel-irqchip=split -device intel-iommu,intremap=on,caching-mode=on
	QEMUFLAGS+=-device iommu-testdev -device edu
else
	QEMUFLAGS+=-machine $(QEMU_MACHINE)
endif

# virtio-fs shares (share=...): the list is parsed in mk/virtiofs.mk.
ifneq ($(strip $(share) $(filter venus,$(gpu))),)
	ifeq ($(UNAME),Linux)
		QEMUFLAGS+=-object memory-backend-memfd,id=mem,size=$(QEMU_MEM)M,share=on
	else
		QEMUFLAGS+=-object memory-backend-file,id=mem,size=$(QEMU_MEM)M,mem-path=/dev/shm,share=on
	endif
	QEMUFLAGS+=-numa node,memdev=mem
endif
ifneq ($(strip $(share)),)
	QEMUFLAGS+=$(foreach i,$(share_idx),-chardev socket,id=vfs$(i),path=$(call vfs_sock,$(i)) -device vhost-user-fs-pci,queue-size=1024,chardev=vfs$(i),tag=$(call vfs_tag,$(i)))
endif

ifeq ($(audio),no)
else ifeq ($(audio),ac97)
	QEMUFLAGS+=-device AC97
else
	QEMUFLAGS+=-device ich9-intel-hda -device hda-output
endif

ifeq ($(net),no)
	QEMUFLAGS+=-net none
else
	ifeq ($(net),rtl8139)
		QEMUFLAGS+=-device rtl8139,netdev=net0
	else ifeq ($(net),virtio)
		QEMUFLAGS+=-device virtio-net,netdev=net0
	else ifeq ($(net),usb-net)
		QEMUFLAGS+=-device usb-net,netdev=net0
	else
		QEMUFLAGS+=-device e1000,netdev=net0,id=nic0
	endif

	EXTRANETARGS=
	ifeq ($(netboot),yes)
		EXTRANETARGS+=,tftp=$(BUILD),bootfile=redox.ipxe
		QEMUFLAGS+=-kernel /usr/lib/ipxe/ipxe-amd64.efi
	endif

	ifneq ($(bridge),)
		QEMUFLAGS+=-netdev bridge,br=$(bridge),id=net0
	else ifeq ($(net),redir)
		FWD_PORTS := 8081 8082 8083 64126
		FWD_FLAGS := hostfwd=tcp::8022-:22,hostfwd=tcp::8080-:80
		FWD_FLAGS2 := $(foreach p,$(FWD_PORTS),,hostfwd=tcp::$(p)-:$(p))
		QEMUFLAGS += -netdev user,id=net0,$(FWD_FLAGS)$(subst $(eval ) ,,$(FWD_FLAGS2))$(EXTRANETARGS)
	else ifeq ($(net),windows)
		QEMUFLAGS+=-netdev user,id=net0$(EXTRANETARGS)
	else
		QEMUFLAGS+=-netdev user,id=net0$(EXTRANETARGS) -object filter-dump,id=f1,netdev=net0,file=$(BUILD)/network.pcap
	endif
endif

ifneq ($(gpu),no)
ifneq ($(strip $(QEMU_DISPLAY)),)
	QEMUFLAGS+=-display $(QEMU_DISPLAY)$(if $(and $(filter egl-headless,$(QEMU_DISPLAY)),$(QEMU_RENDER_NODE)),$(comma)rendernode=$(QEMU_RENDER_NODE))
endif
endif

ifeq ($(gpu),no)
	QEMUFLAGS+=-nographic -vga none
else ifeq ($(gpu),vga)
	ifeq ($(VGA_SUPPORTED),yes)
		QEMUFLAGS+=-vga none -device VGA,id=gpu0
	else
		QEMUFLAGS+=-vga none -device secondary-vga,id=gpu0
	endif
else ifeq ($(gpu),ramfb)
	QEMUFLAGS+=-vga none -device ramfb,id=gpu0
else ifeq ($(gpu),multi)
	ifeq ($(VGA_SUPPORTED),yes)
		QEMUFLAGS+=-vga none -device virtio-vga,id=gpu0,max_outputs=2$(QEMU_GPU_OPTS)
	else
		QEMUFLAGS+=-vga none -device virtio-gpu,id=gpu0,max_outputs=2$(QEMU_GPU_OPTS)
	endif
else ifeq ($(gpu),virtio)
	ifeq ($(VGA_SUPPORTED),yes)
		QEMUFLAGS+=-vga none -device virtio-vga,id=gpu0$(QEMU_GPU_OPTS)
	else
		QEMUFLAGS+=-vga none -device virtio-gpu,id=gpu0$(QEMU_GPU_OPTS)
	endif
else ifeq ($(gpu),virtio-sdl)
	ifeq ($(VGA_SUPPORTED),yes)
		QEMUFLAGS+=-vga none -device virtio-vga,id=gpu0
	else
		QEMUFLAGS+=-vga none -device virtio-gpu,id=gpu0
	endif
else ifeq ($(gpu),venus)
	ifeq ($(VGA_SUPPORTED),yes)
		QEMUFLAGS+=-vga none -device virtio-vga-gl,id=gpu0,hostmem=$(QEMU_GPU_HOSTMEM),blob=true,venus=true
	else
		QEMUFLAGS+=-vga none -device virtio-gpu-gl,id=gpu0,hostmem=$(QEMU_GPU_HOSTMEM),blob=true,venus=true
	endif
else ifeq ($(gpu),virtio-gl)
	ifeq ($(VGA_SUPPORTED),yes)
		QEMUFLAGS+=-vga none -device virtio-vga-gl,id=gpu0
	else
		QEMUFLAGS+=-vga none -device virtio-gpu-gl,id=gpu0
	endif
endif

EXTRA_DISK=$(BUILD)/extra.img
disk?=nvme
ifeq ($(disk),ata)
	QEMUFLAGS+= \
		-drive file=$(DISK),format=raw \
		-drive file=$(EXTRA_DISK),format=raw
else ifeq ($(disk),nvme)
	QEMUFLAGS+= \
		-drive file=$(DISK),format=raw,if=none,id=drv0 -device nvme,drive=drv0,serial=NVME_SERIAL \
		-drive file=$(EXTRA_DISK),format=raw,if=none,id=drv1 -device nvme,drive=drv1,serial=NVME_EXTRA
else ifeq ($(disk),usb)
	QEMUFLAGS+= \
		-drive if=none,id=usbstick,format=raw,file=$(DISK) \
		-device usb-storage,drive=usbstick
else ifeq ($(disk),virtio)
	QEMUFLAGS+= \
		-drive file=$(DISK),format=raw,if=virtio \
		-drive file=$(EXTRA_DISK),format=raw,if=virtio
else ifeq ($(disk),cdrom)
	QEMUFLAGS+= \
		-boot d -cdrom $(DISK) \
		-drive file=$(EXTRA_DISK),format=raw
else ifeq ($(disk),sdcard)
	QEMUFLAGS+=-drive file=$(DISK),if=sd,format=raw
else ifeq ($(disk),windows)
	QEMUFLAGS+=-drive file="$(shell wslpath -w $(WINDOWS_DISK))",format=raw,if=virtio
endif

ifeq ($(gdb),yes)
	QEMUFLAGS+=-d cpu_reset -s -S
else ifeq ($(gdb),nonblock)
	QEMUFLAGS+=-d cpu_reset -s
endif

ifeq ($(UNAME),Linux)
	ifneq ($(kvm),no)
		ifeq ($(QEMU_ON_WINDOWS),1)
			QEMUFLAGS+=-accel whpx,kernel-irqchip=off -cpu Broadwell,x2apic=off
		else
			ifeq ($(gpu),venus)
				ifneq ($(shell grep -m1 -w GenuineIntel /proc/cpuinfo 2>/dev/null),)
					QEMUFLAGS+=-accel kvm,honor-guest-pat=on -cpu host
				else
					QEMUFLAGS+=-enable-kvm -cpu host
				endif
			else
				QEMUFLAGS+=-enable-kvm -cpu host
			endif
		endif
	else
		QEMUFLAGS+=-cpu $(QEMU_CPU)
	endif
endif

ifeq ($(UNAME),Darwin)
    ifneq ($(kvm),no)
        QEMUFLAGS+=-accel hvf -cpu max
    else
        QEMUFLAGS+=-cpu $(QEMU_CPU)
    endif
endif

ifneq ($(PFLASH0),)
	QEMUFLAGS+=-drive if=pflash,format=raw,unit=0,file=$(PFLASH0),readonly=on
endif

ifneq ($(PFLASH1),)
	QEMUFLAGS+=-drive if=pflash,format=raw,unit=1,file=$(BUILD)/fw_vars.bin
endif

.PHONY: qemu-deps

qemu-deps: $(DISK) $(VIRTIOFS_DEPS)

ifeq ($(disk),usb)
else ifeq ($(disk),sdcard)
else ifeq ($(disk),windows)
qemu-deps: $(WINDOWS_DISK)
else
qemu-deps: $(EXTRA_DISK)
endif

qemu-deps:$(FIRMWARE)

qemu-deps:$(QEMU_KERNEL)

qemu-deps: $(PFLASH0)

ifneq ($(PFLASH1),)
qemu-deps: $(BUILD)/fw_vars.bin

.PRECIOUS: $(BUILD)/fw_vars.bin
$(BUILD)/fw_vars.bin: $(PFLASH1)
	cp "$<" "$@"
	chmod +w "$@"
endif

qemu-deps:
	$(if $(PFLASH0_NEEDED),$(if $(PFLASH0),,$(error UEFI firmware needed but not found)),)
	$(if $(PFLASH1_NEEDED),$(if $(PFLASH1),,$(error UEFI variables needed but not found)),)

$(EXTRA_DISK):
	truncate -s 1g $@

$(WINDOWS_DISK): $(BUILD)/harddrive.img
	rm -f $@
	mkdir -p $(shell dirname $@)
	cp "$<" "$@"

$(BUILD)/raspi3bp_uboot.rom:
	wget -O $@ https://gitlab.redox-os.org/Ivan/redox_firmware/-/raw/main/platform/raspberry_pi/rpi3/u-boot-rpi-3-b-plus.bin

$(BUILD)/qemu_uboot.rom:
	wget -O $@ https://gitlab.redox-os.org/Ivan/redox_firmware/-/raw/main/platform/qemu/qemu_arm64/u-boot-qemu-arm64.bin

ifneq ($(strip $(QEMU_VK_DRIVER_FILES)),)
qemu: export VK_DRIVER_FILES:=$(QEMU_VK_DRIVER_FILES)
endif

qemu: qemu-deps
	FUMOUNT='$(FUMOUNT)' scripts/image-mount.sh unmount-if-mounted "$(MOUNT_DIR)"
	$(VIRTIOFS_RUN) $(QEMU) $(QEMUFLAGS)
