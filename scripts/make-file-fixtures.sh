#!/bin/sh
# Rebuild tests/fixtures/files/: one-second clips in every format the "Clean a
# file" decoder opens, cut from the public fixtures (tests/fixtures/README.md).
# The masters are the truth the decode tests compare against; every other
# file is ffmpeg's encoding of a master. Needs ffmpeg with libmp3lame,
# libopus, libvorbis, libx264 and libvpx. Deterministic output (bitexact).
set -eu
cd "$(dirname "$0")/../tests/fixtures"
out=files
mkdir -p "$out"
ff() { ffmpeg -nostdin -v error -y "$@"; }
bx="-fflags +bitexact -flags:a +bitexact -map_metadata -1"
vbx="-flags:v +bitexact"
pic="-f lavfi -i testsrc2=size=64x48:rate=10:duration=1"

# Masters: speech over a fan (48 kHz mono) and, at 44.1 kHz, speech over a
# fan left and over café chatter right (the channels differ on purpose).
ff -ss 1 -t 1 -i noisy_public_48k.flac $bx -c:a pcm_s16le "$out/master_48k_mono.wav"
ff -ss 1 -t 1 -i noisy_public_48k.flac -ss 1 -t 1 -i noisy_cafe_48k.flac \
   -filter_complex "[0:a][1:a]join=inputs=2:channel_layout=stereo,aresample=44100" \
   $bx -c:a pcm_s16le "$out/master_44k_stereo.wav"
m48="$out/master_48k_mono.wav"
m44="$out/master_44k_stereo.wav"

ff -i "$m44" $bx -c:a pcm_s16be "$out/aiff_44k_stereo.aiff"
ff -i "$m44" $bx -c:a flac "$out/flac_44k_stereo.flac"
ff -i "$m44" -af "pan=stereo|c0=c0|c1=c0" $bx -c:a flac "$out/flac_44k_same.flac"
ff -i "$m44" $bx -c:a alac "$out/alac_44k_stereo.m4a"
ff -i "$m44" $bx -c:a libmp3lame -b:a 128k "$out/mp3_44k_stereo.mp3"
ff -i "$m44" $bx -c:a aac -b:a 128k "$out/aac_44k_stereo.m4a"
ff -i "$m44" $bx -c:a libvorbis -q:a 3 "$out/vorbis_44k_stereo.ogg"
ff -i "$m48" $bx -c:a libopus -b:a 64k "$out/opus_48k_mono.opus"
ff -i "$m48" $bx -c:a wmav2 -b:a 64k "$out/wma_48k_mono.wma"
ff $pic -i "$m48" -map 0:v -map 1:a $bx $vbx -c:v libx264 -preset ultrafast -c:a aac -b:a 96k "$out/video_h264_aac.mp4"
ff $pic -i "$m48" -map 0:v -map 1:a $bx $vbx -c:v libx264 -preset ultrafast -c:a aac -b:a 96k "$out/video_h264_aac.mov"
ff $pic -i "$m48" -map 0:v -map 1:a $bx $vbx -c:v libx264 -preset ultrafast -c:a libopus -b:a 64k "$out/video_h264_opus.mkv"
ff $pic -i "$m48" -map 0:v -map 1:a $bx $vbx -c:v libvpx -b:v 50k -c:a libopus -b:a 64k "$out/video_vp8_opus.webm"
# A video without a sound track.
ff $pic $bx $vbx -c:v libx264 -preset ultrafast "$out/video_only.mp4"
# A damaged file: the master three times over as MP3, cut to its first
# 40 % of bytes (damage counts only more than a second before the stated
# end, so the cut needs a longer file than the others).
tmp=$(mktemp -d)
ff -stream_loop 2 -i "$m44" $bx -c:a libmp3lame -b:a 128k "$tmp/long.mp3"
size=$(stat -c %s "$tmp/long.mp3")
head -c $((size * 4 / 10)) "$tmp/long.mp3" > "$out/mp3_44k_cut.mp3"
# The same as AAC in an M4A, its index first (as a phone or ffmpeg's
# +faststart writes it), cut the same way: its index states 3 s.
ff -stream_loop 2 -i "$m44" $bx -c:a aac -b:a 128k -movflags +faststart "$tmp/long.m4a"
size=$(stat -c %s "$tmp/long.m4a")
head -c $((size * 4 / 10)) "$tmp/long.m4a" > "$out/aac_44k_cut.m4a"
# A WAV written as a stream: its sizes say 0xFFFFFFFF (whole, not damaged);
# and a WAV whose header states its size (three times the master), cut to
# its first 40 % (damaged).
ff -i "$m48" $bx -c:a pcm_s16le -f wav - > "$out/wav_48k_streamed.wav"
ff -stream_loop 2 -i "$m48" $bx -c:a pcm_s16le "$tmp/long.wav"
size=$(stat -c %s "$tmp/long.wav")
head -c $((size * 4 / 10)) "$tmp/long.wav" > "$out/wav_48k_cut.wav"
rm -r "$tmp"
# An MP4 whose edit runs past its sound track's end, as some muxers write
# it: the whole track's length (1,023 ms) from after 6,000 frames of
# priming. Whole, not damaged ("Damaged after 0:59" on a 59 s video).
python3 - "$out/aac_44k_stereo.m4a" "$out/aac_44k_edit_overrun.m4a" <<'PY'
import struct, sys
d = bytearray(open(sys.argv[1], "rb").read())
at = d.find(b"elst") + 4 + 8
d[at:at + 8] = struct.pack(">Ii", 1023, 6000)
open(sys.argv[2], "wb").write(d)
PY
# A VBR MP3 without the Xing/LAME header: no stated length, read to its end.
ff -i "$m44" $bx -c:a libmp3lame -q:a 4 -write_xing 0 "$out/mp3_44k_vbr_noxing.mp3"
ls -l "$out"
