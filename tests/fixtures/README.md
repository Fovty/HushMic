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
