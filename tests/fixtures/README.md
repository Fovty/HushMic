# Test fixtures

Public clips used by the tests. All are 48 kHz mono, built by
`docs/demo/make-demos.py` from the sources credited in
`docs/demo/ASSETS.md` (a public-domain LibriVox reading mixed with public
or CC BY noise at +3 dB SNR).

| File | Content |
|------|---------|
| `noisy_public_48k.flac` | speech over a fan |
| `noisy_cafe_48k.flac` | speech over café chatter |
| `noisy_keyboard_48k.flac` | speech over mechanical keyboard typing |
| `golden_public_dpdfnet8.flac` | `noisy_public_48k.flac` through `dpdfnet8_48khz_hr` |

`crates/dpdfnet-ladspa/tests/adaptive_models.rs` runs the three noisy clips
through the adaptive engine with a scripted CPU load (sustained overload,
recovery, a flapping load with stalls) and checks what reaches the output.
`ci/tier1-system-test.sh` loops `noisy_public_48k.flac` through a real
PipeWire graph.

## `files/`: the "Clean a file" decoder

One-second clips cut from `noisy_public_48k.flac` (and `noisy_cafe_48k.flac`
for a second channel), rebuilt by `scripts/make-file-fixtures.sh`. The two
masters (`master_48k_mono.wav`, `master_44k_stereo.wav`) are the truth;
every other file is ffmpeg's encoding of one of them: AIFF, FLAC (plus
`flac_44k_same.flac`, one channel twice), ALAC, MP3, AAC, Vorbis, Opus and
WMA, MP4/MOV/MKV/WebM videos with sound, `video_only.mp4` without, and
`mp3_44k_cut.mp3`, a three-second MP3 cut after its first 40 % (and
`aac_44k_cut.m4a` the same as AAC, its index first).
`wav_48k_streamed.wav` is a WAV written as a stream (sizes 0xFFFFFFFF),
`wav_48k_cut.wav` a three-second one cut the same way,
`mp3_44k_vbr_noxing.mp3` a VBR MP3 without its Xing header, and
`aac_44k_edit_overrun.m4a` the AAC file with its edit list running past the
track's end (as some muxers write it). `crates/hushmic/tests/files_decode.rs`
checks that each decodes starting on the master's first sample and exactly
as long.
