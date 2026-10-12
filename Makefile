PREFIX ?= /usr/local
DESTDIR ?=

all:
	cargo build --release --locked

install:
	@test -f target/release/johncast || { echo "run 'make' first (as your user, not root)"; exit 1; }
	install -Dm755 target/release/johncast $(DESTDIR)$(PREFIX)/bin/johncast

uninstall:
	rm -f $(DESTDIR)$(PREFIX)/bin/johncast

clean:
	cargo clean

.PHONY: all install uninstall clean
