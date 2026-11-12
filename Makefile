PREFIX ?= /usr/local
BINDIR ?= $(PREFIX)/bin

.PHONY: install
install:
	install -d "$(DESTDIR)$(BINDIR)"
	install -m 755 rmux "$(DESTDIR)$(BINDIR)/rmux"
