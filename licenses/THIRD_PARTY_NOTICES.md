# Embedded decoder notices

Spacer embeds libmpv and its decoding dependencies from this upstream binary build:

- Builder: https://github.com/shinchiro/mpv-winbuild-cmake
- Release: https://github.com/shinchiro/mpv-winbuild-cmake/releases/tag/20261014
- Archive: mpv-dev-x86_64-20261014-git-ec38361c56.7z
- Archive SHA-256: 325555b4e0b1d87fb7c92c96010776581599c64e4988052cb9d05e130b52f939
- mpv revision identified by the release: ec38361c56
- mpv project: https://github.com/mpv-player/mpv
- FFmpeg project: https://ffmpeg.org/

mpv and FFmpeg include code under GPL and LGPL terms, and this binary build includes GPL components. Other statically linked dependencies retain their own copyrights and licenses. The pinned archive contains the decoder DLL but no license texts. Spacer embeds this notice, not a complete set of dependency notices, license copies or corresponding source.

This notice is not a complete corresponding-source distribution or a substitute for the exact upstream dependency notices. Before redistributing Spacer, identify the precise source revisions and build configuration used for the bundled decoder and satisfy all applicable notice and corresponding-source requirements. Review GPL compatibility for the application itself. A link to an upstream release is not by itself a complete source offer.

No application license is assigned by this notice. The decoder and its dependencies remain subject to their original license terms.
