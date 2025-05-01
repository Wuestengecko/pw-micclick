pw-micclick
===========

*Play clicks via pipewire when you start to talk, similar to TeamSpeak 3*

Ever started talking and wondered whether people could even hear you?

Ever made a weird noise and wondered whether it went through the mic?

Wonder no more! With this little program, you will hear an audible click when
the microphone's captured volume passes some threshold, and another click when
it stays below the threshold for a brief period of time.

Also features a tray icon.

Pairs very well with a [volume gate and/or noise
suppression](https://github.com/wwmm/easyeffects).

Installation
------------

Clone the repo and build it with `cargo build --release`, then install the
binary from `target/release/pw-micclick` somewhere along your `$PATH`.

Usage
-----

In order to actually hear the clicking, you need to provide sound files via
`--on-sound` and `--off-sound`. Anything that libsndfile can read should work
fine. Also see `--help` for more flags.

To start automatically at login, you can use a systemd user unit like this.
Place it at `~/.config/systemd/user/pw-micclick.service`, then enable it via
`systemctl --user enable --now pw-micclick.service`:

```ini
[Unit]
Requires=pipewire.service pipewire-pulse.service
After=pipewire.service pipewire-pulse.service pipewire-session-manager.service plasma-xembedsniproxy.service

[Service]
Type=exec
ExecStart=/home/<YOUR_USERNAME_HERE>/.local/bin/pw-micclick --on-sound /opt/teamspeak3/sound/default/mic_click_on.wav --off-sound /opt/teamspeak3/sound/default/mic_click_off.wav
Restart=always
RestartSec=10

[Install]
WantedBy=default.target
```
