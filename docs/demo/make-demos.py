#!/usr/bin/env python3
"""Rebuild the README demo clips from their public sources.

Downloads the voice + noise sources documented in ASSETS.md, mixes them at the
documented levels (+3 dB SNR), runs the mixes through the REAL HushMic enhancer
(cargo example `enhance`, so the demos always reflect the current DSP), and
renders the before/after waveform videos.

Usage (from the repo root, after scripts/setup-assets.sh):
    python3 docs/demo/make-demos.py [out-dir]

Outputs demo_{keyboard,fan,cafe}.mp4 plus the before/after WAVs into out-dir
(default: docs/demo/build, gitignored). Requires: ffmpeg, numpy, cargo.
"""

import os
import subprocess as sp
import sys

import numpy as np

REPO = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", ".."))
OUT = os.path.abspath(sys.argv[1]) if len(sys.argv) > 1 else os.path.join(REPO, "docs/demo/build")
SRC = os.path.join(OUT, "src")
FONT = "/usr/share/fonts/truetype/dejavu/DejaVuSans-Bold.ttf"
SR = 48000
VOICE_START, VOICE_DUR = 17.55, 10.75  # "After Love" poem body, see ASSETS.md

SOURCES = {
    "voice.mp3": "https://archive.org/download/spc266_2508_librivox/spc266_afterlove_pac_128kb.mp3",
    "keyboard.wav": "https://upload.wikimedia.org/wikipedia/commons/2/27/Typing_on_Keychron_V1_Ultra_%28Red_Linear_Switch%29.wav",
    "fan.wav": "https://upload.wikimedia.org/wikipedia/commons/9/99/Air_conditioner_hum_%28Gravity_Sound%29.wav",
    "cafe.ogg": "https://upload.wikimedia.org/wikipedia/commons/b/b5/Restaurant_ambience.ogg",
}
# noise file -> (source, offset seconds into the source)
DEMOS = {
    "keyboard": ("keyboard.wav", 1.0, "keyboard noise", "clean voice + keyboard @ SNR +3 dB  •  48 kHz mono"),
    "fan": ("fan.wav", 1.0, "fan noise", "clean voice + fan @ SNR +3 dB  •  48 kHz mono"),
    "cafe": ("cafe.ogg", 4.0, "café chatter", "clean voice + café chatter @ SNR +3 dB  •  48 kHz mono"),
}


def run(*cmd, **kw):
    sp.run(cmd, check=True, capture_output=True, **kw)


def load(path, offset, dur):
    raw = sp.run(
        ["ffmpeg", "-v", "error", "-ss", str(offset), "-t", str(dur), "-i", path,
         "-f", "f32le", "-acodec", "pcm_f32le", "-ac", "1", "-ar", str(SR), "-"],
        capture_output=True, check=True).stdout
    return np.frombuffer(raw, dtype=np.float32).copy()


def rms_db(x):
    return 20 * np.log10(np.sqrt(np.mean(x**2)) + 1e-12)


def norm_rms(x, target_db):
    return x * 10 ** ((target_db - rms_db(x)) / 20)


def write_wav(path, x):
    p = sp.Popen(["ffmpeg", "-v", "error", "-y", "-f", "f32le", "-ar", str(SR), "-ac", "1",
                  "-i", "-", "-c:a", "pcm_f32le", path], stdin=sp.PIPE)
    p.communicate(x.astype(np.float32).tobytes())
    assert p.returncode == 0


def waveform_bg(wav, png, title, footer, color):
    fc = (
        f"aformat=channel_layouts=mono,showwavespic=s=1280x260:colors={color}:filter=peak[wave];"
        f"color=c=0x0d1117:s=1280x420:d=1[bgc];[bgc][wave]overlay=0:116:format=auto[m];[m]"
        f"drawbox=x=0:y=114:w=1280:h=1:color=0x2d333b:t=fill,"
        f"drawbox=x=0:y=377:w=1280:h=1:color=0x2d333b:t=fill,"
        f"drawtext=fontfile={FONT}:text='{title}':fontcolor=0xf9b45c:fontsize=52:x=(w-text_w)/2:y=28,"
        f"drawtext=fontfile={FONT}:text='{footer}':fontcolor=0x8b95a5:fontsize=26:x=(w-text_w)/2:y=386[out]"
    )
    run("ffmpeg", "-y", "-i", wav, "-filter_complex", fc, "-map", "[out]", "-frames:v", "1", png)


def video_half(png, wav, mp4):
    # overlay (not drawbox) for the playhead: only overlay's x expression has `t` = time
    run("ffmpeg", "-y", "-loop", "1", "-framerate", "30", "-i", png, "-i", wav,
        "-filter_complex",
        f"color=c=white@0.75:s=3x260:r=30,format=rgba[ph];"
        f"[0:v][ph]overlay=x='min(main_w-3,main_w*t/{VOICE_DUR})':y=116:shortest=0[v]",
        "-map", "[v]", "-map", "1:a", "-t", str(VOICE_DUR), "-r", "30",
        "-c:v", "libx264", "-pix_fmt", "yuv420p", "-preset", "medium", "-crf", "20",
        "-c:a", "aac", "-b:a", "128k", "-ar", str(SR), "-ac", "1", mp4)


def main():
    os.makedirs(SRC, exist_ok=True)
    for name, url in SOURCES.items():
        dest = os.path.join(SRC, name)
        if not os.path.exists(dest):
            print("downloading", name)
            run("curl", "-fsSL", "-o", dest, url)

    voice = norm_rms(load(os.path.join(SRC, "voice.mp3"), VOICE_START, VOICE_DUR), -20.0)
    n = len(voice)

    for name, (noise_file, offset, label, footer) in DEMOS.items():
        noise = norm_rms(load(os.path.join(SRC, noise_file), offset, VOICE_DUR), -23.0)[:n]
        if len(noise) < n:
            noise = np.pad(noise, (0, n - len(noise)))
        mix = voice + noise  # SNR = -20 - (-23) = +3 dB
        mix = mix * (10 ** (-1 / 20) / np.max(np.abs(mix)))  # flat gain -> peak -1 dBFS
        before = os.path.join(OUT, f"before_{name}.wav")
        after = os.path.join(OUT, f"after_{name}.wav")
        write_wav(before, mix)

        print("enhancing", name)
        run("cargo", "run", "--release", "-q", "--example", "enhance", "-p", "dpdfnet-ladspa",
            "--", before, after, cwd=REPO)

        bg_b = os.path.join(OUT, f"bg_before_{name}.png")
        bg_a = os.path.join(OUT, f"bg_after_{name}.png")
        waveform_bg(before, bg_b, f"BEFORE - {label}", footer, "0x722d8e")
        waveform_bg(after, bg_a, "AFTER - HushMic",
                    "same audio through HushMic (dpdfnet8)  •  48 kHz mono", "0x2d8e57")
        h1 = os.path.join(OUT, f"h1_{name}.mp4")
        h2 = os.path.join(OUT, f"h2_{name}.mp4")
        video_half(bg_b, before, h1)
        video_half(bg_a, after, h2)
        cc = os.path.join(OUT, f"cc_{name}.txt")
        with open(cc, "w") as f:
            f.write(f"file 'h1_{name}.mp4'\nfile 'h2_{name}.mp4'\n")
        run("ffmpeg", "-y", "-f", "concat", "-safe", "0", "-i", cc,
            "-c", "copy", "-movflags", "+faststart", os.path.join(OUT, f"demo_{name}.mp4"))
        print("built", f"demo_{name}.mp4")

    print("done ->", OUT)


if __name__ == "__main__":
    main()
