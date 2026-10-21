# Embedded decoder notices

StarPlayer embeds libmpv and its decoding dependencies from this upstream binary build:

- Builder: https://github.com/shinchiro/mpv-winbuild-cmake
- Release: https://github.com/shinchiro/mpv-winbuild-cmake/releases/tag/20261014
- Archive: mpv-dev-x86_64-20261014-git-ec38361c56.7z
- Archive SHA-256: 325555b4e0b1d87fb7c92c96010776581599c64e4988052cb9d05e130b52f939
- mpv revision identified by the release: ec38361c56
- mpv project: https://github.com/mpv-player/mpv
- FFmpeg project: https://ffmpeg.org/

mpv and FFmpeg include code under GPL and LGPL terms, and this binary build includes GPL components. Other statically linked dependencies retain their own copyrights and licenses. The pinned archive contains the decoder DLL but no license texts. StarPlayer embeds this notice, not a complete set of dependency notices, license copies or corresponding source.

This notice is not a complete corresponding-source distribution or a substitute for the exact upstream dependency notices. Before redistributing StarPlayer, identify the precise source revisions and build configuration used for the bundled decoder and satisfy all applicable notice and corresponding-source requirements. Review GPL compatibility for the application itself. A link to an upstream release is not by itself a complete source offer.

No application license is assigned by this notice. The decoder and its dependencies remain subject to their original license terms.

## Optional export tool

StarPlayer releases offer a separate optional download of the statically linked
`ffmpeg.exe` from Gyan's FFmpeg 9.0.2 full build, including the dav1d AV1 decoder.
It is not embedded in the application. FFprobe and FFplay are not distributed.
Media inspection uses the existing bundled libmpv. A compatible user-provided
FFmpeg installation can be used instead of downloading the optional package.

- Builder: https://github.com/GyanD/codexffmpeg
- Release: https://github.com/GyanD/codexffmpeg/releases/tag/9.0.2
- Archive: `ffmpeg-9.0.2-full_build.7z`
- Archive SHA-256: `f0e46253c70dfe902bac915dfb4224f0cf1b7c6eeab9da2ccc9a5581f9a71b13`
- FFmpeg source revision reported by the release: `946fcce07b`
- FFmpeg source: https://github.com/FFmpeg/FFmpeg/commit/946fcce07b

The optional ZIP includes the archive's license and README alongside FFmpeg
and this notice. Downloads require user consent and match the package manifest
compiled into the running StarPlayer version.
This build includes GPL components, including the H.264 encoder. Review the
exact build configuration and all linked dependency licenses before shipping.
These notices and links do not replace the obligation to provide the
complete corresponding source, build scripts, and any required source offer
for the exact binaries distributed. Existing decoder licensing obligations
above still apply.
