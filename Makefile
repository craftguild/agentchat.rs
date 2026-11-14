ARTIFACT ?= agentchat
RELEASE_GROUP ?= agentchat
RELEASE_ARGS ?= --apply
override RELEASE_ARGS := --group $(RELEASE_GROUP) $(RELEASE_ARGS)
ACCEPTANCE_ARGS ?= --gate standalone
export NODE_PATH := $(CURDIR)/web/node_modules
include standards/make/common.mk

.NOTPARALLEL:
.PHONY: setup-generator assets
setup: setup-generator
setup-generator:
	python3 tools/chigiri_contract.py --setup
assets:
	npm run build --prefix web
