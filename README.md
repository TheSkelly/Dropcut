# DropCut

A desktop video editor that does the boring parts for you: it removes pauses,
transcribes your speech on your own computer, makes captions you can edit,
and mutes or bleeps swear words. Built with Rust (Tauri) and React.

## Install

Grab the latest installer from the repo's **Releases** page.

**Windows:** run `DropCut_x.y.z_x64-setup.exe`. Because the app isn't
code-signed, Windows may say "Windows protected your PC": click **More info**,
then **Run anyway**. Speech recognition runs on your NVIDIA GPU through
Vulkan, which comes with the normal NVIDIA driver (keep it up to date).

**Mac (Apple Silicon):** open the `.dmg` and drag DropCut into Applications.
The first time, **right-click DropCut → Open** (double-clicking will refuse
because the app isn't notarized). If macOS says the app "is damaged", run this
once in Terminal: `xattr -cr /Applications/DropCut.app`.

## Using it

1. **Drop a video** onto the window (or press Ctrl/Cmd+O). Pauses are detected
   straight away and shown in orange on the waveform.
2. **Click any highlighted region** to keep it or cut it again. Tune detection
   on the right: *Shortest pause to cut* and *Breathing room*. With
   **Preview edits** on, playback skips the cuts just like the export will.
3. **Transcribe.** The first time, DropCut downloads the speech model you
   picked (Large v3 Turbo, about 550 MB, is the best choice with a GPU). After
   that everything runs offline.
4. **Edit by text.** In the transcript, click a word to jump there; shift-click
   to select a range and press **Delete** to cut it; double-click a word to fix
   a mistake in the captions.
5. **Profanity.** Swear words are found automatically, highlighted in yellow,
   and masked in captions (`f***`). Choose *Mute* or *Bleep*, add your own
   words, or allow ones you don't mind. Click a chip to leave one uncensored.
6. **Export.** You get an MP4 plus a matching `.srt` caption file (upload it to
   YouTube/TikTok, or drop it on your Premiere/Resolve timeline). Export uses
   the NVIDIA or Apple hardware encoder when available.

Your settings are remembered between sessions.

## Roadmap

| Phase | Features | Status |
|---|---|---|
| 1 | Silence removal, transcription + editable captions, profanity filter, installers | **Done** |
| 2 | Animated burned-in captions (word highlight styles), auto zoom / punch-ins | Next |
| 3 | Bad-take (repeat) removal, chapters, viral short finder (9:16 reframe), B-roll from Pexels | Planned |
| 4 | Podcast mode: multi-track speaker detection and automatic camera switching | Planned |
| + | Premiere / Resolve XML export so edits stay editable | Planned |

## Project layout

```
crates/dropcut-core   The engine: ffmpeg, silence detection, timeline, Whisper,
                      profanity, captions, rendering. Unit tested.
crates/dropcut-cli    `dropcut` command-line tool (same engine, handy for batches)
src-tauri             Desktop app backend (Tauri commands wrapping the engine)
src                   React interface
dev                   Browser preview harness with mocked backend (dev only)
.github/workflows     Builds the Windows and Mac installers
```

## Developing

Needs Rust (stable), Node 22, CMake and a C++ compiler; ffmpeg on `PATH`.

```sh
npm install
# ffmpeg sidecars for the app (named with your platform's target triple):
#   src-tauri/binaries/ffmpeg-<triple>[.exe] and ffprobe-<triple>[.exe]
npm run tauri dev                     # run the app (CPU speech recognition)
npm run tauri dev -- --features metal # Mac with GPU
cargo test -p dropcut-core            # engine tests
```

Command line:

```sh
cargo build --release -p dropcut-cli --features whisper
./target/release/dropcut cut input.mp4 -o output.mp4 --srt output.srt --bleep
./target/release/dropcut --help
```

### Releasing

Bump `version` in `src-tauri/tauri.conf.json`, `package.json` and the root
`Cargo.toml`, then:

```sh
git tag v0.2.0 && git push origin v0.2.0
```

GitHub Actions builds both installers (about 20 to 30 minutes) and publishes
them as a release.

## Third-party components

ffmpeg (GPL build, bundled as a separate program), whisper.cpp via
`whisper-rs`, and OpenAI Whisper model weights (MIT, downloaded on first use).
DropCut is private software for personal use.
