# VLC audio comparison

## What VLC documents

VLC's Windows MMDevice output uses the Windows Multimedia Device API, not a
special bass-restoration filter. LibVLC exposes output-device selection, stereo
channel modes, software volume and optional equalization as separate controls.
Its documented nominal volume is 100%, or 0 dB. These controls do not imply that
VLC and mpv always negotiate identical stream formats or use identical resamplers.

Sources:

- [VideoLAN MMDevice output documentation](https://wiki.videolan.org/Documentation:Modules/mmdevice/)
- [LibVLC 3 audio controls](https://videolan.videolan.me/vlc-3.0/group__libvlc__audio.html)
- [VLC 3 audio documentation](https://docs.videolan.me/vlc-user/desktop/3.0/en/basic/audio.html)
- [mpv audio options](https://mpv.io/manual/stable/#audio)
- [Windows endpoint roles](https://learn.microsoft.com/en-us/windows/win32/api/mmdeviceapi/nf-mmdeviceapi-immdeviceenumerator-getdefaultaudioendpoint)
- [Windows session process identification](https://learn.microsoft.com/en-us/windows/win32/api/audiopolicy/nf-audiopolicy-iaudiosessioncontrol2-getprocessid)

## Findings from the reported comparison

Read-only checks of the currently reported clip and the running players found:

- The source has one AAC-LC audio track, two channels, at 44.1 kHz.
- Spacer's null-output decoder probe preserved stereo at 44.1 kHz. Its track
  gain was 1, master volume 100, playback speed 1, and extra audio filters empty.
  This probe does not measure the physical output format or the headset sound.
- The installed VLC version was 3.0.24. Its saved settings did not enable an EQ,
  spatializer, headphone filter or volume-normalizer filter. Runtime settings
  can differ from saved settings.
- VLC's visible software-volume control was at 100%.
- Both live Windows audio sessions used the same render endpoint and had Windows
  session volume 1. The endpoint's shared mix format was eight channels at
  48 kHz, 32 bits. This is an endpoint format, not proof of either application's
  submitted stream format.
- The endpoint was a Logitech PRO X 2 LIGHTSPEED. The user also reported that
  Windows Media Player sounded flat while the browser sounded fuller. Turning
  off headset surround/spatial processing eliminated the difference, but made
  all applications sound flat. This isolates the reported contrast to that
  processing path, rather than proving a fault in stereo decoding.

Logitech documents separate multi-channel surround and **DTS Super Stereo**
processing, including application-dependent behavior:

- [G HUB surround settings](https://support.logi.com/hc/en-us/articles/13086293456151-How-to-customize-surround-sound-with-Logitech-G-HUB)
- [DTS Super Stereo and Firefox](https://support.logi.com/hc/en-sg/articles/35914540641815-How-to-activate-DTS-Surround-Sound-for-Firefox-using-G-HUB)

Do not turn off G HUB permanently if its processed sound is the desired result.
The temporary off comparison was a diagnostic, not the intended solution.

The file is already stereo. Surround downmixing of this source cannot explain
the reported difference, and the checks did not find a different Windows device
or session-volume attenuation. No additional EQ or arbitrary bass boost was
added in response to this comparison.

Spacer now offers an opt-in **Headset compatibility** output in the mixer. It
uses the embedded decoder's OpenAL output with stereo channels, instead of the
usual WASAPI output. It does not install a decoder, add an EQ or change G HUB
settings. This is a backend comparison, not an implementation of Logitech DTS.
The muted physical-output probe on this headset successfully switched between
WASAPI stereo at 48 kHz and OpenAL stereo reported at 44.1 kHz, preserving mute.
OpenAL's reported input format does not prove its internal device format or that
the headset applies the desired effects. Listening confirmation is required.

Restore the desired G HUB effects and compare **Headset compatibility** with
**Stereo** using the same clip. If it does not restore the desired sound,
the next useful comparison is VLC's
Messages window at verbosity 2, alongside Spacer's terminal diagnostics, with
both applications using the same track, position and nominal volume. Compare
the chosen output module, submitted channel layout, sample format and rate,
resampling and enabled filters. Headset-driver processing can also be
application-specific. A null-output probe cannot test that processing.

## Reproduce the decoder probe

The probe uses the bundled libmpv, not installed VLC, mpv or ffmpeg executables.
It opens a caller-supplied file with null audio output and writes a JSON report.
Use explicit local paths in PowerShell:

```powershell
$env:SPACER_AUDIO_PROBE = 'C:\Videos\example.mp4'
$env:SPACER_AUDIO_PROBE_OUTPUT = "$env:USERPROFILE\.spacer\audio-probe.json"
cargo test diagnose_audio_file -- --ignored
```

For the separate physical-backend probe, use the same explicit paths and run
`cargo test diagnose_headset_backends -- --ignored`. This opens the headset
output but sets mute before loading the media and verifies that both routes stay
muted. It is not a perceptual sound-quality test.

This is an opt-in diagnostic, separate from the interactive native-output and
throughput tests. Do not publish reports without reviewing their media metadata.
