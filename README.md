# Spacer

A Windows x86_64 Rust/GPUI media player with embedded libmpv GPU output.

## Self-contained Windows build

The Windows x86_64 executable includes the decoder. Users do not need to install mpv, ffmpeg, codec packs, or decoder DLLs. Playback and hover thumbnails both use the embedded library. No external decoder executables are launched and no decoder libraries are searched for on `PATH`.

On first launch, Spacer creates a directory in the user's home:

```text
~/.spacer/
  runtime/
    <bundle-id>/
      libmpv-2.dll
      notices/
        THIRD_PARTY_NOTICES.md
```

On Windows this is normally `%USERPROFILE%\.spacer`. The cache is persistent and versioned. Extraction publishes a complete bundle atomically, and cached resources are checksum-verified before first use in each process. Windows loads the decoder from that absolute path, with dependency searches limited to its directory and Windows System32. A modified or damaged cache produces an error; remove that bundle directory and restart to restore it from the EXE.

The finished app needs no network connection for local playback or previews. It still uses normal Windows APIs, system drivers, and a writable home directory. The embedded package currently supports Windows x86_64 only.

## Build and run

Install Rust and the native build prerequisites for GPUI. A clean build downloads the pinned decoder archive, about 32 MB, from GitHub and verifies its SHA-256 before extraction and embedding. This is a build-time download only. The EXE is larger because it contains the decoder.

```sh
cargo run
cargo run -- "C:/Videos/example.mp4"
cargo build --release
```

Ship `target/release/spacer.exe`; no separate decoder files need to accompany it. For builds without network access, supply the same pinned `.7z` archive using the build-time `SPACER_MPV_ARCHIVE` environment variable. It must have the expected checksum. Cargo dependencies must also be available locally.

Pinned build:

- Release: https://github.com/shinchiro/mpv-winbuild-cmake/releases/tag/20261014
- Archive: `mpv-dev-x86_64-20261014-git-ec38361c56.7z`
- SHA-256: `325555b4e0b1d87fb7c92c96010776581599c64e4988052cb9d05e130b52f939`

## Formats and audio tracks

The embedded build handles common formats such as MP4/M4V, MKV, MOV, AVI, WebM, MPEG/TS, WMV, FLV, Ogg, MP3, AAC/M4A, FLAC, WAV, and Opus. Support depends on the codecs included in that build. Encrypted media, DRM-protected streams, damaged files, and every codec variant are not guaranteed.

Audio-only files are supported. Embedded audio tracks each have an independent gain slider and are mixed into one synchronized output. The first track starts at 100%; the rest start at 0%. Raising another slider plays it alongside the first. Gains are not normalized, so loud tracks can clip when mixed. Lower track gains to leave headroom. Changing gains rebuilds the audio graph and may briefly interrupt audio. External audio-file attachment is not implemented.

## Controls

| Action                           | Shortcut                      |
| -------------------------------- | ----------------------------- |
| Open media                       | Ctrl+O                        |
| Play/pause                       | Space or K                    |
| Play                             | P                             |
| Pause                            | Pause key                     |
| Stop, rewind and pause           | S                             |
| Seek backward/forward 10 seconds | Left/Right or J/L             |
| Seek backward/forward 60 seconds | Shift+Left/Right or Shift+J/L |
| Start/end                        | Home/End                      |
| Seek to 0–90%                    | 0–9                           |
| Previous/next frame while paused | `,` / `.`                     |
| Master volume                    | Up/Down                       |
| Mute                             | M                             |
| Change speed                     | Shift+`,` / Shift+`.`         |
| Reset speed                      | R                             |
| Select audio track               | `[` / `]`                     |
| Selected track volume            | `-` / `+` or `=`              |
| Toggle selected track            | A                             |
| Fullscreen                       | F                             |
| Show/hide shortcut help          | H or ?                        |
| Hide shortcut help               | Escape                        |

Hover over the timeline for a thumbnail and its requested timestamp. Click or drag to seek. A separate, silent decoder handles previews without changing playback position. Requests are debounced and superseded requests are canceled. Audio-only files have no thumbnail.

On Windows, media play/pause, stop, previous, and next keys work while Spacer is the foreground application. Previous/next seek by 10 seconds because there is no playlist. Windows system volume keys keep their normal behavior. Background media-session integration is not implemented.

Frame stepping uses mpv's frame commands rather than an assumed frame rate. Backward stepping can be slower because the decoder must seek and reconstruct preceding frames.

Logical pause always suppresses audio, including mpv's temporary resume during
stepping. Explicit play restores the user's mute choice. Master volume and track
gains remain unchanged.

## Native video and diagnostics

GPUI owns the controls and an application-owned child HWND. libmpv receives that
HWND through `wid` and presents video with `vo=gpu`, D3D11 and `hwdec=auto`.
Unsupported hardware decoding falls back to software decoding with GPU output.
Main playback does not copy frames into GPUI images. The 100 ms UI timer updates
controls, not video presentation. libmpv owns playback timing and synchronization.
Only thumbnails and software-renderer tests use software render contexts.

Native child windows cannot compose with GPUI overlays. Transport controls,
errors, shortcut help and the reserved thumbnail area sit outside the video
panel. The thumbnail area uses vertical space even when no preview is shown.
The child cannot take input focus, so GPUI keeps keyboard control. Panel bounds
use physical pixels calculated from GPUI layout and its display scale. Windows
hides the child when its parent is minimized. Closing stops and joins playback
before destroying the host; teardown services synchronous native messages.

The GPUI window and its kit `Root` use transparent backgrounds. Only the video
rectangle is left unpainted; header, gutters and controls remain opaque. This
matters on Windows, where GPUI's DirectComposition layer can sit above child
HWNDs. An opaque root hides the video even while audio and GPU output run.

Launching from an attached terminal enables the shared `Print` logger. Its
bounded asynchronous queue drops excess messages rather than blocking playback.
Logs include decoder loading, commands, audio suppression/restoration, panel
bounds and DPI, libmpv diagnostics, and five-second playback/drop statistics.
Decoder warning/error logs are diagnostics, not fatal playback failures.
Authoritative load failures and command failures retain decoder context.

The EXE embeds `libmpv-2.dll` and the notices in `licenses/`. It does not embed
mpv or ffmpeg executables, codec packs, or source archives.

## Validation

```sh
cargo check
cargo test
cargo fmt -- --check
cargo test software_60fps_render_throughput -- --ignored
cargo test native_gpu_playback_steps_and_60fps_throughput -- --ignored
```

Tests exercise the actual embedded decoder using video/audio fixtures written in Rust. No ffmpeg or system-installed decoder is needed for the tests. They cover software-rendered frame pixels, paused forward/backward stepping, simultaneous audio-track selection and gain configuration, seek and file-replacement races, preview decoding and cancellation, and extraction integrity. Audio tests use null output; physical-device output and audible mix quality still need listening tests.

The native opt-in test needs an interactive Windows desktop and D3D11. It checks
GPU output, real-time 60 fps fixture playback and dropped-frame counters, silent
paused forward/backward steps, muted/unmuted restoration, physical panel resizing
and shutdown. The separate software benchmark requires at least 90 distinct
frames from a 120-frame 60 fps clip. Neither test proves smooth presentation of
compressed media on every GPU or measures acoustic silence.

Before this integration, `cargo check` passed with one unused-viewport warning
and tests passed with 24 successes and one ignored benchmark. Main GPUI image
delivery used a 33 ms poll, imposing about a 30 fps UI ceiling. The software
backend throughput benchmark itself passed its 90-of-120-frame threshold.

Manual release checks still required:

- Compressed 24/30/60 fps MP4/H.264, MKV and WebM, audio-only files and
  multiple tracks. Compare visible playback against dropped-frame logs.
- Listen during repeated forward/backward steps, timeline dragging and resume.
- Resize, fullscreen, multi-monitor DPI, minimize/restore, click focus and close.
- Verify GPUI controls and thumbnails never cross the native video rectangle.
- Check terminal output and run the packaged EXE offline on a machine with no
  installed mpv or ffmpeg.

These manual checks are not claimed as completed. Native texture sharing,
background media sessions, DRM and non-Windows packaging remain out of scope.

A GPUI-window visual check with a generated blue AVI confirmed that native video
and a hover thumbnail are visible after the composition fix. This does not
replace the compressed-format, acoustic and hardware coverage listed above.

## Third-party licenses

The bundled decoder includes GPL components. A third-party notice is embedded
and extracted into `notices`; the pinned archive provides no license texts.
License copies and corresponding source are not bundled. Embedding the DLL does
not remove license obligations. Before distribution, identify the exact upstream
build and provide the required license texts, dependency notices and
corresponding source for libmpv and its dependencies. An upstream release URL
alone does not satisfy corresponding-source requirements. This implementation
does not choose an application license or claim the EXE is ready for compliant
redistribution.
