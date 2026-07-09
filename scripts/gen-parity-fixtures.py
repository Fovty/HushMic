#!/usr/bin/env python3
"""Regenerate the committed public parity fixtures (tests/fixtures/*.flac).

Builds a deterministic noisy input from PUBLIC sources (the same LibriVox
voice + Wikimedia Commons fan noise the README demos use, pinned URLs), runs
it through the CURRENT engine via the `enhance` example, and stores both as
int16 FLAC (lossless, bit-exact to decode everywhere, small enough to commit).

Only rerun this when the DSP is INTENTIONALLY changed and the new output has
been validated (listen + check the correlation numbers it prints): the whole
point of the committed golden is that CI fails when the audio path changes
unintentionally. Requires: ffmpeg, numpy, cargo (with assets provisioned).

Usage: python3 scripts/gen-parity-fixtures.py
"""

import os
import subprocess as sp
import tempfile

import numpy as np

REPO = os.path.abspath(os.path.join(os.path.dirname(__file__), ".."))
FIX = os.path.join(REPO, "tests", "fixtures")
SR = 48000
VOICE_URL = "https://archive.org/download/spc266_2508_librivox/spc266_afterlove_pac_128kb.mp3"
FAN_URL = "https://upload.wikimedia.org/wikipedia/commons/9/99/Air_conditioner_hum_%28Gravity_Sound%29.wav"
VOICE_START, DUR = 17.55, 15.0  # pause-rich poem body; pauses expose the denoising
FAN_START = 1.0


def run(*cmd, **kw):
    return sp.run(cmd, check=True, capture_output=True, **kw)


def load_f32(path, offset=0.0, dur=None):
    cmd = ["ffmpeg", "-v", "error", "-ss", str(offset)]
    if dur is not None:
        cmd += ["-t", str(dur)]
    cmd += ["-i", path, "-f", "f32le", "-acodec", "pcm_f32le", "-ac", "1", "-ar", str(SR), "-"]
    return np.frombuffer(run(*cmd).stdout, dtype=np.float32).copy()


def rms_db(x):
    return 20 * np.log10(np.sqrt(np.mean(x**2)) + 1e-12)


def norm_rms(x, target_db):
    return x * 10 ** ((target_db - rms_db(x)) / 20)


def to_i16(x):
    # deterministic quantization in numpy (no encoder dither)
    return np.clip(np.round(x * 32767.0), -32768, 32767).astype(np.int16)


def write_flac_i16(path, x_i16):
    p = sp.Popen(["ffmpeg", "-v", "error", "-y", "-f", "s16le", "-ar", str(SR), "-ac", "1",
                  "-i", "-", "-c:a", "flac", path], stdin=sp.PIPE)
    p.communicate(x_i16.tobytes())
    assert p.returncode == 0


def write_wav_f32(path, x):
    p = sp.Popen(["ffmpeg", "-v", "error", "-y", "-f", "f32le", "-ar", str(SR), "-ac", "1",
                  "-i", "-", "-c:a", "pcm_f32le", path], stdin=sp.PIPE)
    p.communicate(x.astype(np.float32).tobytes())
    assert p.returncode == 0


def corr(a, b):
    n = min(len(a), len(b))
    a, b = a[:n].astype(np.float64), b[:n].astype(np.float64)
    a -= a.mean()
    b -= b.mean()
    return float(np.dot(a, b) / (np.linalg.norm(a) * np.linalg.norm(b) + 1e-30))


def main():
    os.makedirs(FIX, exist_ok=True)
    with tempfile.TemporaryDirectory() as tmp:
        voice_src = os.path.join(tmp, "voice.mp3")
        fan_src = os.path.join(tmp, "fan.wav")
        run("curl", "-fsSL", "-o", voice_src, VOICE_URL)
        run("curl", "-fsSL", "-o", fan_src, FAN_URL)

        voice = norm_rms(load_f32(voice_src, VOICE_START, DUR), -20.0)
        n = len(voice)
        fan = load_f32(fan_src, FAN_START)  # ~13 s source, tiled to length
        fan = norm_rms(np.resize(fan, n), -23.0)
        mix = voice + fan  # SNR +3 dB, same recipe as the README demos
        mix = mix * (10 ** (-1 / 20) / np.max(np.abs(mix)))  # peak -1 dBFS

        noisy_i16 = to_i16(mix)
        noisy_flac = os.path.join(FIX, "noisy_public_48k.flac")
        write_flac_i16(noisy_flac, noisy_i16)

        # Golden = the current (validated) engine's streaming output over the
        # EXACT int16 samples the test will decode.
        noisy_wav = os.path.join(tmp, "noisy.wav")
        write_wav_f32(noisy_wav, noisy_i16.astype(np.float32) / 32768.0)
        golden_wav = os.path.join(tmp, "golden.wav")
        run("cargo", "run", "--release", "-q", "--example", "enhance", "-p", "dpdfnet-ladspa",
            "--", noisy_wav, golden_wav, cwd=REPO)
        golden = load_f32(golden_wav)
        golden_i16 = to_i16(np.clip(golden, -1.0, 1.0))
        golden_flac = os.path.join(FIX, "golden_public_dpdfnet8.flac")
        write_flac_i16(golden_flac, golden_i16)

        base = corr(noisy_i16, golden_i16)
        quant = corr(golden, golden_i16.astype(np.float64) / 32768.0)
        print(f"fixtures written: {noisy_flac} ({os.path.getsize(noisy_flac)} B), "
              f"{golden_flac} ({os.path.getsize(golden_flac)} B)")
        print(f"corr(noisy, golden)   = {base:.4f}  <- passthrough bug would score ~this")
        print(f"corr(golden_f32, i16) = {quant:.7f} <- quantization cost (must be ~1)")
        print("test threshold is 0.99: it must sit clearly ABOVE the first number.")


if __name__ == "__main__":
    main()
