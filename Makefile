PREFIX ?= /usr/local
BINDIR ?= $(PREFIX)/bin

.PHONY: install test
install:
	cargo build --release
	install -d "$(DESTDIR)$(BINDIR)"
	install -m 755 target/release/rmux "$(DESTDIR)$(BINDIR)/rmux"

test:
	cargo test
	cargo build
	python3 -m unittest -v test_rmux.py
