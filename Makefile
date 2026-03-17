.PHONY: build install uninstall docker-sanity

BINARIES := omar omar-computer omar-slack
INSTALL_DIR := $(HOME)/.cargo/bin

build:
	cargo build --release

install: build
	install -d $(INSTALL_DIR)
	install $(addprefix target/release/,$(BINARIES)) $(INSTALL_DIR)/

uninstall:
	rm -f $(addprefix $(INSTALL_DIR)/,$(BINARIES))

docker-sanity:
	./scripts/docker-sanity.sh
