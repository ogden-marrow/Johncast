# Johncast

The simplest terminal podcast client. Source only, no binary shipped: you build it yourself.

    make
    sudo make install     # PREFIX=/usr/local by default; DESTDIR and PREFIX honored
    johncast

Features: save podcasts by RSS feed URL, play episodes (needs `mpv`), and see which episodes you've listened to.

Settings live in `$XDG_CONFIG_HOME/johncast/settings.json` (default `~/.config/johncast/settings.json`).
