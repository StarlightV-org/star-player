# Portable updates

StarPlayer checks public GitHub Releases in `StarlightV-org/star-player`. Only
newer stable Windows x86_64 releases are accepted. No GitHub token is shipped.
MinIO and an installer are not required.

Settings > About contains separate automatic-check and automatic-download
preferences, both enabled by default, plus a manual check/download/restart
button. Release builds check after ten seconds and every six hours while open.
Development builds only check manually. Disabling downloads affects future
downloads; an in-progress download finishes. Restarts always require a click.

Network work runs on a worker thread. The title bar shows checking, download
percentage, verification, errors/retry, and restart availability. The running
version does not change until restart. Failed checks never block playback.

## One-time repository setup

1. Install Bun 1.4.0 or newer. Signing uses built-in crypto, with no extra packages.
2. Run `bun scripts/release.ts --generate-key` locally. Do not commit its
   output or run key generation in public CI logs.
3. Store the public value as the GitHub repository Actions variable
   `STAR_PLAYER_UPDATE_PUBLIC_KEY`.
4. Create a GitHub environment named `release`. Store the private value there
   as the Actions secret `STAR_PLAYER_UPDATE_PRIVATE_KEY`. Restrict deployment
   tags and configure required reviewers as appropriate for the repository.
5. Keep an offline backup of the private key. Do not casually rotate the public
   key: existing clients trust the old key and cannot verify a new key without
   an explicitly designed transition release.

Official executables embed the public key at compile time. Builds without that
environment variable still work, but reject update installation. The first
official release must already contain the key to accept later updates.

## Publish

Run `bun run publish:release` to select a version, create an annotated `vX.Y.Z`
tag, and push the current branch and tag atomically to `origin`. Use
`bun run publish:release --remote NAME` for another configured remote.

The script compares StarPlayer's version in Cargo.lock with the highest stable
version tag, fetching remote tags first. If unchanged, it increases the patch
by one. If you already set a newer Cargo.lock version, it uses that value.
Without an existing stable tag, it publishes the current version unchanged.
It synchronizes Cargo.toml and Cargo.lock, commits version changes when needed,
and refuses downgrades or a branch that does not contain the last release.

Commit application changes before running it. Only package-version edits in
the Cargo files may be uncommitted; unrelated changes are never auto-committed.
If a push fails, the script retains the local commit/tag and prints the exact
push command to retry. Do not rerun the script to retry that release.

`.github/workflows/release.yml` builds and tests
on Windows, transfers the executable to an isolated publishing job, signs the
metadata, uploads every asset to a draft release, and publishes it last.

Required release assets:

- `star-player-windows-x86_64.exe`, a standalone executable, not an installer.
- `update.json`, containing version, exact target, asset name, size and SHA-256.
- `update.json.sig`, a base64 Ed25519 signature over the exact metadata bytes.
- `SHA256SUMS`, for manual checksum verification.

Do not modify published artifacts or reuse a version tag. If publishing fails
after draft creation, inspect and remove only that incomplete draft before
rerunning the workflow. Windows Authenticode signing is not configured here;
if added, sign the executable before producing update metadata and checksums.
Update signatures authenticate releases but do not remove SmartScreen warnings.

## Replacement and recovery

Downloads are bounded by signed size and staged beneath `.star-player/updates`.
The app verifies the metadata signature and full executable hash. It rejects
wrong targets, old versions, prereleases and incomplete payloads.

On a restart click, StarPlayer saves playback state and stages another verified
copy beside the installed executable. A temporary copy of the current app runs
in helper mode, waits for the original process to exit, verifies again, retains
`previous.exe`, then replaces the executable using a same-volume rename and
relaunches it. A read-only folder fails before quitting. A lock held by another
process leaves the old executable intact and reports an error.

If a video was open when restart was requested, the helper passes its path to
the new process. The existing playback-resume preference controls restoration.

If creating the new process fails, the helper attempts to restore and relaunch
the previous executable. A crash after successful process creation is not
automatically detected or rolled back. The previous executable is retained in
the `.star-player-update-*` folder beside the app for manual recovery. Verified
download and helper folders are also retained; automatic cache cleanup is not
yet implemented. User settings and media are never replaced.

## Release validation

Unit tests cover signature tampering, wrong keys, unsupported targets,
downgrades, invalid sizes, corrupted/truncated payloads, default settings,
duplicate checks, Windows destination locks and failed-relaunch restoration.
`bun run test:release` tests the publishing signature contract and raw-key
compatibility. Existing base64 Ed25519 keys remain valid; no rotation is needed.
Before shipping, test downloaded version N to N+1 on a clean Windows VM,
including UI responsiveness, saved state, offline/rate-limited requests,
interrupted downloads, read-only destinations, multiple running instances,
file locks, helper timeout and failed relaunch. Validate the version after
restart. These interactive/platform tests are not replaced by CI unit tests.
