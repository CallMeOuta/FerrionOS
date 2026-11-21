
ifneq ($(SKIP_CHECK_TOOLS),1)
ifeq ($(shell which rustup),)
$(error rustup not found, install from "https://rustup.rs/")
endif
ifeq ($(shell which cbindgen),)
$(error cbindgen not found, install from crates.io or from your package manager)
endif
ifeq ($(shell which nasm),)
$(error nasm not found, install from your package manager)
endif
ifeq ($(shell which just),)
$(error 'just' not found, install from crates.io or from your package manager)
endif
endif
