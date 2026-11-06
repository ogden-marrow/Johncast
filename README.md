# Johncast

Terminal podcast player with a full-screen now-playing view: artwork, MP3 chapters
(with their pictures and links), and Overcast sync. Needs `mpv` for audio.

    make && sudo make install

Keys are mpv's (press `?` in the player for the full list).

Images use kitty/WezTerm/Ghostty/iTerm2/foot/GNOME Terminal/Ptyxis graphics when detected, otherwise block
characters. Force a mode with `JOHNCAST_IMAGES=kitty|sixel|iterm|blocks`.
