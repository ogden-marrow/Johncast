PREFIX ?= /usr/local
DESTDIR ?=

all:
	cargo build --release --locked

install: all
	install -Dm755 target/release/johncast $(DESTDIR)$(PREFIX)/bin/johncast

uninstall:
	rm -f $(DESTDIR)$(PREFIX)/bin/johncast

clean:
	cargo clean

.PHONY: all install uninstall clean
