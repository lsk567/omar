.PHONY: build install uninstall docker-up docker-down docker-shell docker-dashboard docker-sanity

BINARIES := omar omar-computer omar-slack
INSTALL_DIR := $(HOME)/.cargo/bin

build:
	cargo build --release

install: build
	install -d $(INSTALL_DIR)
	install $(addprefix target/release/,$(BINARIES)) $(INSTALL_DIR)/

uninstall:
	rm -f $(addprefix $(INSTALL_DIR)/,$(BINARIES))

docker-up:
	docker compose up -d omar

docker-down:
	docker compose down

docker-shell:
	./scripts/docker-shell.sh

docker-dashboard:
	./scripts/docker-dashboard.sh

docker-sanity:
	./scripts/docker-sanity.sh
