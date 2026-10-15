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

Install Rust and the native build prerequisites for GPUI. A clean build downloads the pinned decoder archive, about 32 MB, from GitHub and verifies its SHA-256 before extraction. The build compresses runtime resources with Zstandard before embedding them in the EXE. This is a build-time download only.

On first use, Spacer streams decompressed resources into the runtime cache and verifies their original size and SHA-256 before publishing the bundle. Later launches reuse the verified cache without decompression. The loaded decoder is byte-for-byte identical to the upstream DLL, so compression does not change playback or hardware acceleration. Existing valid caches remain compatible. The extracted decoder's disk size is unchanged.

```sh
cargo run
cargo run -- "C:/Videos/example.mp4"
cargo build --release
```

Ship `target/release/spacer.exe`; no separate decoder files need to accompany it. For builds without network access, supply the same pinned `.7z` archive using the build-time `SPACER_MPV_ARCHIVE` environment variable. It must have the expected checksum. Cargo dependencies must also be available locally.

## Local game clip library

Launching without a file opens the clip library. On the first such launch,
Spacer asks whether to allow NVIDIA Overlay folder discovery. If approved,
it next asks whether to search for local Steam games and their icons. Both
permission choices are saved in `%USERPROFILE%\.spacer\config.json`. Launching with a
file skips the prompt. The title-bar cog opens **Settings**, where you can
add or remove library folders, toggle automatic NVIDIA folder discovery, or
use **Detect now**. Disabling discovery leaves listed folders monitored.
Removing a folder does not delete its files or watched history. Unavailable
folders remain listed and resume scanning when their drives reconnect.

Detection reads `%LOCALAPPDATA%\NVIDIA Corporation\NVIDIA Overlay\GallerySettings.json`
and adds `settings.currentDirectoryV2` to the `clip_directories` array. Spacer
does not change NVIDIA settings or move recordings. Missing settings or folders
produce an error and leave manual folder selection available.
Enabled discovery checks on startup and during periodic reconciliation.
**Detect now** does not change the automatic-discovery preference.

Game folders keep their hierarchy and show the latest clip's filesystem creation
time and a recursive clip count. Folders without clips anywhere in their hierarchy
are hidden. Sort folders by newest clip, name, or clip count.
Clips within each folder are newest first. Creation time falls back to modification
time if unavailable.

### Offline Steam game icons

After approving NVIDIA Overlay discovery, approve the separate Steam game
search prompt to start a background search. Declining either prompt does not
prevent manual folder setup or custom icons. There is no Steam search button
in the library. Use **Search now** in Settings to repeat a search. Steam
permission is saved; if it has not been granted, Search now asks first.
Turn off **Allow local Steam game searches** to revoke permission without
removing cached icons. Spacer reads local Steam
installation metadata, game manifests across Steam libraries, cached artwork,
and Steam `.url` shortcuts on the desktop and in the Start menu. It makes no
network requests, launches no games, and never changes Steam files. There are
no automatic Steam scans at startup. Found games and icons update the UI while
the background search runs. Warnings and completion appear as notifications,
not blocking dialogs. Settings explains the Steam image sources and cache.
If Steam cannot be located, use **Choose Steam folder…** in Settings.

Unique folder-name matches receive an icon automatically. Matching ignores
case, whitespace, and punctuation, but does not use fuzzy search. Automatic
matching applies only to folders directly beneath a configured recording root.
Nested folders keep their normal folder icon unless assigned manually. Games
without usable local artwork also keep the normal icon.

Right-click a folder for **Set custom icon…**, **Choose game icon…**, or
**Restore assigned icon**. The custom-icon picker allows PNG, JPEG, ICO, and
EXE files only. Selecting an EXE extracts its first icon without running it.
The extracted icon is cached as PNG, so the executable is not needed afterward.
Custom images are limited to 16 MB and 4096 pixels per dimension; this file-size
limit does not apply to executables. The searchable game picker includes
previously detected games, even after uninstalling them. Manual overrides
survive rescans. Restoring clears the override and shows the automatic icon,
or the normal folder icon if no automatic match exists.

Icons are normalized to PNG and copied into `.spacer/game-icons/images`.
Steam ICO files containing RGB or indexed PNG frames are converted to RGBA.
`catalog.json` stores discovered Steam games and `folders.json` stores folder
assignments by absolute path. These icons do not count toward the thumbnail
cache limit and remain after a game or the original custom image is removed.
Missing or damaged images fall back to the normal folder icon. A fresh scan
can repair an installed game's icon. Malformed icon JSON files are reported
and not overwritten. Moving a recording folder changes its assignment key.

Directories are watched recursively. A startup scan and periodic reconciliation
also catch offline changes and reconnected drives. Directory links are not followed.
Only recognized video extensions are listed; screenshots and audio-only files
remain accessible through **Open file**. Thumbnails use the bundled decoder,
load on demand, and cache in `.spacer/thumbnails`. The disk cache is limited to
2,000,000 bytes. Startup and new writes remove the oldest generated thumbnails
first; evicted thumbnails are regenerated when needed.

Click a clip row without modifiers to open the separate player. Use the checkboxes
to select clips, Ctrl-click to toggle selection, or Shift-click for a range.
Ctrl+A selects the current folder's clips. Left/right edge buttons
move through the original folder list; hover to preview the destination. Navigation
does not wrap or cross folders. Deleted entries are skipped, and new recordings
enter the list after returning to the library. **Library** restores the folder,
selection, and scroll position.
Escape clears the list selection and its range-selection anchor. Returning to
the library exits fullscreen; minimizing or switching applications does not.

Files opened with **Open file**, Ctrl+O, or a command-line path also support
Previous/Next. Their queue contains videos in the same directory, sorted by
filename, without scanning subdirectories or adding the directory to the library.
Each edge button appears only when the cursor is near it and a destination exists.

Playback beginning marks a clip watched. `.spacer/watched.csv` contains
`sha256,identity_version,watched_at,track_gains` records with UTC watched timestamps
and a JSON map of audio-track IDs to gains. An empty timestamp means unwatched,
without discarding saved audio settings. SHA-256 identity combines the immediate
parent folder name and full filename as separate fields. For example,
`Desktop/clip.mp4` and `Game/clip.mp4` have different identities, while matching
folder names and filenames under different root paths still share state.
Watched checks do not read video contents or wait for thumbnails. Files with the
same identity share watched status and track settings. Moving a file between
different folder names changes its identity. Existing two-column filename-hash
records remain readable and migrate as corresponding paths become known. Legacy
rows remain available for offline folders. Older content-hash records cannot be
matched without reading media contents and are retained without reinterpretation.
Bulk **Mark watched** and **Mark unwatched** update the same history.
Bulk controls share the sorting toolbar without shifting the clip list. Narrow
windows use a selection-actions menu. Right-clicking a selected clip acts on the
selection; right-clicking an unselected clip targets only that clip without
changing selection. The context menu has icons for watched status, single-file
rename, and destructive-colored Delete below a separator.

Use **Rename** in a clip's context menu or click the player's title-bar filename to edit
the filename stem while keeping its extension. Renaming transfers watched
status and track settings to
the new hash, updates navigation and selection, and restores an open player's
position and pause state. Existing destinations are never overwritten. A settings
save failure attempts to roll back the rename and reports any incomplete recovery.
The filename is centered in the title bar. Hovering over it reveals a pencil.
The filename and Settings cog are interactive, not window-drag regions.
In fullscreen, the editable
title appears in a small top-center capsule alongside the auto-hiding controls.
Press Enter in the rename field to save, or Escape to cancel.

Bulk **Delete** and the Delete key ask for confirmation before moving clips to
the Windows Recycle Bin. Failures are reported without permanently deleting files.
Configuration and history updates use atomic replacement; malformed existing
files are reported and not overwritten. Running multiple Spacer instances that
edit the same configuration/history simultaneously is not supported.

The Windows title bar is drawn by GPUI Kit with the Spacer icon/name and window
controls. It is hidden in fullscreen; native video occupies the space below it.
The player's Library and Open file buttons are at the upper left. The info button
shows a concise file summary, video codec/resolution/frame rate, audio/subtitle
track details, chapters and useful tags. Decoder internals and duplicate fields
are omitted. Errors use GPUI Kit notifications instead of inline banners or
player error boxes. Repeated playback failures do not notify on every poll.
The audio mixer and file-information panels close when clicking outside them.
Their toggle buttons still open and close them, and clicks inside the panels
do not dismiss them. An open mixer keeps the player controls visible.
Keyboard shortcuts are listed in Settings > Keybinds
and remain accessible with H or ?.

Pinned build:

- Release: https://github.com/shinchiro/mpv-winbuild-cmake/releases/tag/20261014
- Archive: `mpv-dev-x86_64-20261014-git-ec38361c56.7z`
- SHA-256: `325555b4e0b1d87fb7c92c96010776581599c64e4988052cb9d05e130b52f939`

## Formats and audio tracks

The embedded build handles common formats such as MP4/M4V, MKV, MOV, AVI, WebM, MPEG/TS, WMV, FLV, Ogg, MP3, AAC/M4A, FLAC, WAV, and Opus. Support depends on the codecs included in that build. Encrypted media, DRM-protected streams, damaged files, and every codec variant are not guaranteed.

Audio-only files are supported. Embedded audio tracks each have an independent
gain slider and are mixed into one synchronized output. New tracks start at 100%.
Saved per-file gains, including zero-gain disabled tracks, are restored before the
initial audio mix. Master volume and mute are global and saved in `config.json`.
Slider writes are debounced and pending settings flush when leaving media or
closing. Gains are not normalized, so loud tracks can clip when mixed. Lower track
gains to leave headroom. Gain changes update the running filters without restarting
playback. External audio-file attachment is not implemented.

## Controls

Settings opens inside the main window, with Clip Library, Theme, Keybinds and
About pages in the sidebar. The default shortcuts below can be changed in
Settings > Keybinds, including the foreground media keys. Windows system volume
keys are not intercepted.

Click the video to play or pause. Double-click it to maximize or restore the
window. A double-click does not also toggle playback; single clicks wait for the
Windows double-click interval. The fullscreen button and F still toggle actual
fullscreen.

The mouse's Back/Forward side buttons retrace visited folders, library and video
views, like browser history. Library entries restore their folder, selection and
scroll offset. Opening another folder or video after going Back clears the forward
history. These buttons do not seek or step through the folder's video list.

| Action                           | Shortcut                      |
| -------------------------------- | ----------------------------- |
| Open media                       | Ctrl+O                        |
| Open settings                    | Ctrl+,                        |
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
| Reload custom theme              | Ctrl+Shift+T                  |

Hover over the timeline for a thumbnail and its requested timestamp. Click or drag to seek. A separate, silent decoder handles previews without changing playback position. Requests are debounced and superseded requests are canceled. Audio-only files have no thumbnail.

On Windows, media play/pause, stop, previous, and next keys work while Spacer is the foreground application. Previous/next media keys seek by 10 seconds. Windows system volume keys keep their normal behavior. Background media-session integration is not implemented.

Frame stepping uses mpv's frame commands rather than an assumed frame rate. Backward stepping can be slower because the decoder must seek and reconstruct preceding frames.

Logical pause always suppresses audio, including mpv's temporary resume during
stepping. Explicit play restores the user's mute choice. Master volume and track
gains remain unchanged.

## Themes

Spacer keeps the original dark plum and rose palette by default. On launch it
creates `~/.spacer/themes/spacer-original.json` if missing, without overwriting
existing edits. The default file uses hex colors. The old `~/.spacer/theme.json`
is left untouched and is no longer loaded.

Choose a theme in Settings > Theme to apply it immediately. The selector shows
each file's `name` field. Spacer remembers the selection in `appearance.json`.
The themes folder is watched for additions, deletions, renames and content edits,
including editors that replace a file on save. Changes to the active theme apply
automatically. If it disappears, Spacer switches to Spacer original.

Create new theme copies the active theme to `custom.json`, then `custom-2.json`,
and so on, and selects the copy. The theme table has per-file Use, Rename and
Show file actions. Renaming an inactive theme does not change the active theme.
Rename updates both `name` and the filename.
For example, `Midnight Rose` becomes `midnight-rose.json`. Duplicate names and
filenames are blocked without regard to letter case. Open themes folder lets you
edit the color values in your preferred editor.

Colors accept `#RRGGBB`, `#RRGGBBAA`, or OKLCH arrays with lightness, chroma,
hue and optional alpha. The default file lists all supported color names.
Partial themes inherit unspecified colors from the original palette:

```json
{
  "name": "My theme",
  "colors": {
    "primary": "#be123c",
    "primary_foreground": "#fff1f2",
    "background": "#100b10",
    "card": "#211821"
  }
}
```

Controls, floating panels, borders and thumbnails use these theme colors.
Invalid live edits keep the last working appearance and show an error in Theme
settings. On startup, an invalid selected theme leaves the embedded original
palette active. Files are never repaired or overwritten automatically.
Theme files are limited to 64 KiB and require a nonempty `name`.

## Custom keybindings

Settings > Keybinds groups actions by context, including actions without a default
key. Unbound rows show `-` and muted text. The Reference column shows namespaced
action references such as `player.pause`; these are also accepted in
`keybinds.json`, alongside the existing compact action IDs.
Filter by action, reference, keystrokes or context. The action picker is searchable
by name or reference. Double-click a row to edit it, or use
Create keybinding to add another shortcut. Record up to four keystrokes or type
a sequence such as `ctrl-k ctrl-s`. Escape can be recorded as a binding too.
Sequences wait one second between keys. An exact binding that is also a sequence
prefix waits until that timeout before running.

Choose a predefined context or enter an expression using `&&`, `||`, `!` and
parentheses. Available contexts are Application, Player, ClipLibrary, Settings,
TextInput, Dialog, KeybindEditor, paused, playing, fullscreen, folder and selected.
For example, `Player && (paused || !fullscreen)`. Input fields and modal dialogs
require bindings that explicitly include TextInput or Dialog, respectively.
More specific contexts take precedence; user bindings win ties with defaults.
The editor warns about overlapping shortcuts and sequence prefixes.

Only user additions, overrides and explicit removals are saved in
`~/.spacer/keybinds.json`. Defaults remain in the application. Removing a default
stores a disabled entry so it stays removed after restarting. Reset action
removes that action's changes; Reset all leaves an empty list. The shortcut-help
panel also reflects the effective bindings. There is no JSON editor in settings.
Malformed keymap files are reported and left unchanged. Repair the file and
restart Spacer before saving further changes.

## Headset audio routing

Playback always uses the bundled OpenAL headset-compatibility output. There is
no output-mode selector. mpv downmixes surround sources
to stereo before handing them to Windows, rather than relying on a headset's
advertised virtual surround layout. Downmix normalization is enabled to reduce
clipping, and decoder-side downmixing is disabled so mpv handles the conversion.
No EQ, bass boost or loudness effect is added. Track-mix gains remain independent.

Diagnostics report the audio backend, device, source channels and output
channels. OpenAL does not emulate Logitech DTS. Keep your desired G HUB effects
enabled; whether they apply still depends on the headset driver.

Stereo routing is a workaround for a possible layout mismatch, not a confirmed
diagnosis of the reported thin headset sound. Compare the same media with VLC.
Windows spatial audio and vendor headset processing can still
affect the result. The six-channel fixture test verifies stereo output and
preservation of volume, gains and pause state, not perceptual equivalence to VLC.

The subsequent [VLC comparison](docs/audio-comparison.md) found that the reported
clip is already stereo and both players use the same Windows endpoint at full
session volume. The user confirmed that disabling headset surround/spatial
processing removed the difference, while making all applications sound flat.
A surround downmix change cannot fix that source. The desired processed sound
is not claimed as restored; the comparison document covers the compatibility
route, vendor guidance and opt-in bundled-decoder probes.

Floating controls, thumbnails, the mixer and help panels block pointer input
from reaching the video behind them. Clicking a panel does not also play/pause
or maximize the window. The seek buttons use the `rotate-ccw-clock` icon, mirrored
for the forward direction and embedded alongside the default icons.

## Native video and diagnostics

GPUI owns the controls and an application-owned child HWND. libmpv receives that
HWND through `wid` and presents video with `vo=gpu`, D3D11 and `hwdec=auto`.
Unsupported hardware decoding falls back to software decoding with GPU output.
Main playback does not copy frames into GPUI images. The 100 ms UI timer updates
controls, not video presentation. libmpv owns playback timing and synchronization.
Only thumbnails and software-renderer tests use software render contexts.

The video host fills the player area below the custom title bar without padding
or a reserved control strip. It is hidden while browsing the library.
Video keeps its original aspect ratio; mismatched window sizes
can still have letterboxing. Compact translucent controls float over the video
and disappear after three seconds of idle playback. Mouse movement or keyboard
input brings them back. Controls stay visible while paused or dragging a slider.
Open mixer and shortcut panels hide with the controls during idle playback and
reappear on mouse movement or pause. The mixer button opens individual track
sliders; thumbnails appear above the timeline without taking permanent space.

The child cannot take input focus, so GPUI keeps keyboard control. Panel bounds
use physical pixels calculated from GPUI layout and its display scale. Windows
hides the child when its parent is minimized. Closing stops and joins playback
before destroying the host; teardown services synchronous native messages.

The window uses three layers: a permanent opaque black native background, native
video nested inside that background, and GPUI controls/library above both.
The background has its own redirected bitmap at full opacity and resizes with
the Windows client area before the next GPUI frame. It stays visible during file
replacement, decoder startup/shutdown, and returning to the library.

The GPUI window and its kit `Root` remain transparent so they do not cover native
video. The library paints an opaque theme background above the native backing;
floating controls paint translucent backgrounds. On the current Windows backend,
GPUI's DirectComposition layer displays controls and thumbnails above the native
children without copying video frames. An opaque GPUI root would hide the video.
Other GPUI backends need separate composition testing.

Release builds use the Windows GUI subsystem and do not open a console when
launched from Explorer. They reuse the parent's terminal when one is available.
Debug builds retain the console subsystem for development.

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
cargo test opaque_background_survives_video_transitions -- --ignored
cargo test library_history_rename_audio_and_fullscreen -- --ignored --test-threads=1
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
- Verify floating controls, the mixer, shortcuts and thumbnails remain visible
  above native video, including after fullscreen and DPI changes.
- Check terminal output and run the packaged EXE offline on a machine with no
  installed mpv or ffmpeg.

These manual checks are not claimed as completed. Native texture sharing,
background media sessions, DRM and non-Windows packaging remain out of scope.

The isolated-store desktop regression test covers folder Back/Forward, Escape
deselection, renaming an open file with saved gains and a paused position, updated
history paths, and exiting fullscreen when returning to the library. It does not
replace listening tests or compressed-format and multi-monitor checks.

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
