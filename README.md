# Johncast

The simplest terminal podcast client, with a nice arrow-key interface (ratatui).
Source only, no binary shipped: you build it yourself.

    make
    sudo make install     # PREFIX=/usr/local by default; DESTDIR and PREFIX honored
    johncast

Features: save podcasts by RSS feed URL, play episodes (needs `mpv`) with resume
where you left off, and see which episodes you've listened to.

Keys: `↑↓` move (wraps around), or just type a number to jump to it, `Enter` open / play,
`Space` toggle listened, `a` add feed, `d` remove, `Esc` back, `q` quit. A shortcut cheat sheet for mpv prints before playback.

Settings live in `$XDG_CONFIG_HOME/johncast/settings.json` (default `~/.config/johncast/settings.json`).
