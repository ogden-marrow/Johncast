# Johncast

Terminal podcast player with a full-screen now-playing view: artwork, MP3 chapters
(with their pictures and links), and Overcast sync. Needs `mpv` for audio.

    make && sudo make install

Images use kitty/WezTerm/Ghostty/iTerm2/foot graphics when detected, otherwise block
characters. Force a mode with `JOHNCAST_IMAGES=kitty|sixel|iterm|blocks`.
