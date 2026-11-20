# Configuration file for VirtualBox, it creates a VirtualBox virtual machine

virtualbox: $(BUILD)/harddrive.img
	echo "Delete VM"
	-$(VBM) unregistervm FerrionOS --delete; \
	if [ $$? -ne 0 ]; \
	then \
		if [ -d "$$HOME/VirtualBox VMs/FerrionOS" ]; \
		then \
			echo "FerrionOS directory exists, deleting..."; \
			$(RM) -rf "$$HOME/VirtualBox VMs/FerrionOS"; \
		fi \
	fi
	echo "Delete Disk"
	-$(RM) harddrive.vdi
	echo "Create VM"
	$(VBM) createvm --name FerrionOS --register
	echo "Set Configuration"
	$(VBM) modifyvm FerrionOS --memory 2048
	$(VBM) modifyvm FerrionOS --vram 32
	if [ "$(net)" != "no" ]; \
	then \
		$(VBM) modifyvm FerrionOS --nic1 nat; \
		$(VBM) modifyvm FerrionOS --nictype1 82540EM; \
		$(VBM) modifyvm FerrionOS --cableconnected1 on; \
		$(VBM) modifyvm FerrionOS --nictrace1 on; \
		$(VBM) modifyvm FerrionOS --nictracefile1 "$(ROOT)/$(BUILD)/network.pcap"; \
	fi
	$(VBM) modifyvm FerrionOS --uart1 0x3F8 4
	$(VBM) modifyvm FerrionOS --uartmode1 file "$(ROOT)/$(BUILD)/serial.log"
	$(VBM) modifyvm FerrionOS --usb off # on
	$(VBM) modifyvm FerrionOS --keyboard ps2
	$(VBM) modifyvm FerrionOS --mouse ps2
	$(VBM) modifyvm FerrionOS --audio-driver $(VB_AUDIO)
	$(VBM) modifyvm FerrionOS --audiocontroller hda
	$(VBM) modifyvm FerrionOS --audioout on
	$(VBM) modifyvm FerrionOS --nestedpaging on
	echo "Create Disk"
	$(VBM) convertfromraw $< $(BUILD)/harddrive.vdi
	echo "Attach Disk"
	$(VBM) storagectl FerrionOS --name ATA --add sata --controller IntelAHCI --bootable on --portcount 1
	$(VBM) storageattach FerrionOS --storagectl ATA --port 0 --device 0 --type hdd --medium $(BUILD)/harddrive.vdi
	echo "Run VM"
	$(VBM) startvm FerrionOS
