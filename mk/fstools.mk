
fstools: $(FSTOOLS_TAG) $(FSTOOLS)

GOING_TO_PODMAN_AGAIN?=0

REDOXFS_TREE=$(shell find components/redoxfs -not -path '*/target/*' 2>/dev/null)

# These tools run inside Podman if it is used, or on the host if Podman is not used
$(FSTOOLS): $(REDOXFS_TREE) recipes/core/installer/recipe.toml $(wildcard recipes/core/installer/*.patch) | prefix $(CONTAINER_TAG) $(FSTOOLS_TAG)
ifeq ($(PODMAN_BUILD),1)
ifeq ($(FSTOOLS_IN_PODMAN),1)
	$(PODMAN_RUN) make $@
else
	$(MAKE) $@ PODMAN_BUILD=0 SKIP_CHECK_TOOLS=1 GOING_TO_PODMAN_AGAIN=1
endif
else
	rm -rf $@ $@.partial
	mkdir -p $@.partial
	ln -s ../../recipes $@.partial/recipes
	ln -s ../../components $@.partial/components
	$(MAKE) fstools_fetch PODMAN_BUILD=$(GOING_TO_PODMAN_AGAIN)

	cd $@.partial && \
		export CARGO_TARGET_DIR=../$@-target && \
		$(HOST_CARGO) install --root . --path recipes/core/installer/source --locked $(INSTALLER_FEATURES) && \
		$(HOST_CARGO) install --root . --path components/redoxfs --locked $(REDOXFS_FEATURES)

	mv $@.partial $@
	touch $@
endif

fstools_fetch: $(FSTOOLS_TAG) FORCE
ifeq ($(PODMAN_BUILD),1)
	$(PODMAN_RUN) make $@
else
	$(REPO_BIN) fetch host:installer
endif

CARGO_OFFLINE_FLAG=
ifeq ($(REPO_OFFLINE),1)
CARGO_OFFLINE_FLAG=--offline
endif

$(FSTOOLS_TAG): $(CONTAINER_TAG)
ifeq ($(PODMAN_BUILD),1)
	$(PODMAN_RUN) make $@
else
	$(HOST_CARGO) build --manifest-path Cargo.toml --release --locked $(CARGO_OFFLINE_FLAG)
	mkdir -p $(@D)
	touch $@
endif

repo_editor: $(FSTOOLS_TAG)
ifeq ($(PODMAN_BUILD),1)
	$(PODMAN_RUN) make $@
else
	CONFIG=$(FILESYSTEM_CONFIG) $(REPO_BIN)_editor
endif

fstools_clean: FORCE
	rm -rf target
	rm -rf $(FSTOOLS)
	rm -rf $(FSTOOLS)-target
	rm -f $(FSTOOLS_TAG)
