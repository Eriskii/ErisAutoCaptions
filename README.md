# ErisAutoCaptions

Live captions for anything your Mac is playing or hearing, like Live Caption on a Pixel. It lives in
the menu bar and shows captions in a draggable overlay styled after ErisMacShell's dock. Speech
recognition runs entirely on your Mac, using [Moonshine](https://github.com/moonshine-ai/moonshine).

## Install

```sh
make install    # into ~/Applications
open ~/Applications/ErisAutoCaptions.app
```

Building needs Rust, on Apple Silicon, with macOS 26 or later. The first build downloads Moonshine's
library. The first time you turn captions on, it downloads the model you've chosen.

## Using it

Click the captions icon in the menu bar:

- **Turn Captions On/Off**.
- **Source**: System Audio, a microphone or other input device, or a window.
- **Model**: Small or Big.
- **Language**: English, Arabic, Chinese, German, Japanese, Spanish, Tagalog or Vietnamese. Pick the
  one being spoken; it doesn't detect languages.
- **Move Captions Back to the Middle**, if the overlay has gone somewhere awkward.

Hover over the overlay and it opens upward with the same Source, Model and Language choices and a Turn
Off button, plus a curved handle in its bottom right corner. Drag the handle to resize: sideways for
width, up and down for one to eight lines. Drag anywhere else on the body to move it. It stays where
and how you leave it, and clicks beside it go through to whatever's underneath.

Models revise their guess as they hear more, so a word only appears once two guesses in a row agree on
it. Text on screen then holds still, at the cost of about half a second of delay.

| Model | English | German, Japanese, Spanish | Arabic, Chinese, Tagalog, Vietnamese |
|---|---|---|---|
| Small | Tiny, 43 MB | Tiny, 31 MB | Tiny, 31 MB |
| Big | Medium, 267 MB | Small, 116 MB | Tiny, 31 MB |

Moonshine reports an average word error rate on the Open ASR Leaderboard of 12.00% for Tiny and 6.65%
for Medium in English. On an M5 Pro, while someone is talking, Big takes about 1.1 GB of memory and a little over one
CPU core, Small about 280 MB and under half a core.

### Windows are really apps

macOS mixes sound per process, not per window, so picking a window captions everything its app plays:
two browser windows can't be told apart. It follows the app's helper processes, which is where browsers
and Electron apps make their sound, and picks up new ones within two seconds.

Window titles need Screen Recording permission. Without it, each app is listed once by name, and
captioning works the same.

### Permissions

macOS asks the first time you caption each kind of source: **System Audio Recording** for system audio
and windows, **Microphone** for input devices. Without the system audio permission macOS delivers
silence rather than an error, so the overlay just stays on "Listening…". Check System Settings, Privacy
& Security.

A build signed ad hoc, which is the default, has to be allowed again after each rebuild. Set
`ERISAUTOCAPTIONS_SIGN` to a signing identity to keep the permission.

## Settings

Everything in the menu is kept in `~/.config/erisautocaptions/config.toml`. Models are in
`~/Library/Application Support/ErisAutoCaptions/models`.

## From the command line

```sh
erisautocaptions transcribe talk.wav --model small   # caption a file
erisautocaptions sources                             # list what can be listened to
erisautocaptions snapshot overlay.png                # draw the overlay without showing a window
```

## License

GPL-3.0-or-later. Moonshine's library and the streaming models used here are MIT licensed.
