//! ffmpeg for Save: writing the cleaned sound in the original's format,
//! and a video with its picture untouched. Found on PATH (the Flatpak
//! runtime ships one); without it, Save writes FLAC (WAV stays WAV) and a
//! video's sound only.

use super::decode::which;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

#[derive(Clone, Debug)]
pub struct Ffmpeg {
    pub ffmpeg: PathBuf,
    pub ffprobe: Option<PathBuf>,
    encoders: Vec<String>,
    /// The input option that names a raw input's channel layout:
    /// `-ch_layout` from ffmpeg 5.1 on, `-channel_layout` before (Ubuntu
    /// 22.04 ships 4.4, which refuses `-ch_layout` and ends at once).
    pub layout_option: &'static str,
}

/// What Save needs to know about an original's sound streams and what
/// else it holds.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Probe {
    /// Every sound stream, in ffmpeg's order (an OBS recording: one per
    /// source). The first's bit rate is the whole file's when it is the
    /// file's one stream and states none of its own.
    pub sounds: Vec<Sound>,
    /// A video stream that is not a cover picture.
    pub video: bool,
    /// Data streams (an MP4's timecode, a camera's telemetry) and
    /// attachments (a Matroska file's fonts): copied along.
    pub data: bool,
    pub attachments: bool,
    /// How many streams of each kind it has (pictures with covers): each
    /// one's own tags are copied by its place.
    pub counts: Counts,
}

impl Probe {
    /// Its `track`th sound stream (0: the first), or an empty one.
    pub fn sound(&self, track: usize) -> Sound {
        self.sounds.get(track).cloned().unwrap_or_default()
    }

    /// Each sound stream's title ("Mic", "Desktop Audio"; empty without).
    pub fn titles(&self) -> Vec<String> {
        self.sounds.iter().map(|s| s.title.clone()).collect()
    }
}

/// Streams of each kind but sound.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Counts {
    pub video: usize,
    pub subtitle: usize,
    pub data: usize,
    pub attachment: usize,
}

/// One sound stream of an original.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Sound {
    pub codec: String,
    /// Bits per second (lossy streams), when stated.
    pub bit_rate: Option<u64>,
    /// ffmpeg's name for the channel layout ("stereo", "5.1(side)").
    pub layout: Option<String>,
    /// How many channels it has, when stated.
    pub channels: Option<usize>,
    /// The depth it stores (lossless and PCM streams), when stated.
    pub bits: Option<u32>,
    /// Its title tag ("Mic/Aux" in an OBS recording); empty without one.
    pub title: String,
}

impl Ffmpeg {
    /// The one on PATH, with its ffprobe next to it.
    pub fn find() -> Option<Ffmpeg> {
        Ffmpeg::at(&which("ffmpeg")?)
    }

    pub fn at(ffmpeg: &Path) -> Option<Ffmpeg> {
        let out = Command::new(ffmpeg)
            .args(["-hide_banner", "-encoders"])
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .ok()?;
        if !out.status.success() {
            return None;
        }
        let encoders = parse_encoders(&String::from_utf8_lossy(&out.stdout));
        let probe = ffmpeg.with_file_name("ffprobe");
        // The raw input's own options say which name this ffmpeg takes.
        let raw = Command::new(ffmpeg)
            .args(["-hide_banner", "-h", "demuxer=f32le"])
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
            .unwrap_or_default();
        Some(Ffmpeg {
            ffmpeg: ffmpeg.to_path_buf(),
            ffprobe: probe.is_file().then_some(probe),
            encoders,
            layout_option: layout_option(&raw),
        })
    }

    pub fn has_encoder(&self, name: &str) -> bool {
        self.encoders.iter().any(|e| e == name)
    }

    pub fn probe(&self, path: &Path) -> Option<Probe> {
        let out = Command::new(self.ffprobe.as_ref()?)
            .args(["-v", "error", "-show_entries"])
            .arg(
                "stream=index,codec_type,codec_name,bit_rate,channels,channel_layout,\
                 bits_per_raw_sample,sample_fmt\
                 :stream_disposition=attached_pic:stream_tags=title:format=bit_rate",
            )
            .args(["-of", "default=nw=1"])
            .arg(path)
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .ok()?;
        out.status
            .success()
            .then(|| parse_probe(&String::from_utf8_lossy(&out.stdout)))
            .flatten()
    }
}

/// When an original's sound starts, in seconds of the file's own time:
/// ffmpeg moves every stream of an input by `file` (its first moment), so
/// a Save gives the cleaned sound each track's first decoded sample's time
/// and the original its own, and nothing moves (F10).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Starts {
    pub file: f64,
    /// Each sound track's first decoded sample (None: none in its first
    /// second, or no time stated), in the probe's order.
    pub sounds: Vec<Option<f64>>,
}

impl Ffmpeg {
    /// [`Starts`]: the file's first second of sound decoded.
    pub fn starts(&self, path: &Path) -> Option<Starts> {
        let out = Command::new(self.ffprobe.as_ref()?)
            .args(["-v", "error", "-select_streams", "a", "-show_entries"])
            .arg("stream=index:frame=stream_index,best_effort_timestamp_time:format=start_time")
            .args(["-read_intervals", "%+1", "-of", "default=nw=1"])
            .arg(path)
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .ok()?;
        out.status
            .success()
            .then(|| parse_starts(&String::from_utf8_lossy(&out.stdout)))
            .flatten()
    }
}

/// ffprobe's frames (each a `stream_index` and its time), the sound
/// streams' `index`es in order, and the format's `start_time`.
fn parse_starts(text: &str) -> Option<Starts> {
    let mut first: Vec<(u32, Option<f64>)> = Vec::new();
    let mut frame: Option<u32> = None;
    let mut order: Vec<u32> = Vec::new();
    let mut file = None;
    for line in text.lines() {
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        match k {
            "stream_index" => frame = v.parse().ok(),
            "best_effort_timestamp_time" => {
                if let Some(i) = frame.take() {
                    if !first.iter().any(|f| f.0 == i) {
                        first.push((i, v.parse().ok()));
                    }
                }
            }
            // a program lists its streams again (MPEG-TS): once each
            "index" => order.extend(v.parse::<u32>().ok().filter(|i| !order.contains(i))),
            "start_time" => file = v.parse::<f64>().ok(),
            _ => {}
        }
    }
    let sounds = order
        .iter()
        .map(|i| first.iter().find(|f| f.0 == *i).and_then(|f| f.1))
        .collect();
    Some(Starts {
        file: file.filter(|f| f.is_finite())?,
        sounds,
    })
}

/// The probe's channel layout as a name every ffmpeg takes back. A layout
/// without a name comes as "1 channels (FL)" or "3 channels (FL+FR+LFE)",
/// which ffmpeg 4.4 (Ubuntu 22.04) refuses: its channels, "FL+FR+LFE",
/// mean the same to old and new. None when there is nothing to keep.
fn layout_name(v: &str) -> Option<String> {
    if v.is_empty() || v == "unknown" {
        return None;
    }
    let listed = v
        .split_once(" channels (")
        .filter(|(n, _)| n.parse::<u32>().is_ok())
        .and_then(|(_, rest)| rest.strip_suffix(')'));
    match listed {
        // names, or a mask ("2 channels (0x1234)"): 4.4 and 6.x parse both
        Some(names) if !names.is_empty() && !names.contains(' ') => Some(names.to_string()),
        // nothing usable in the parentheses: the count decides
        Some(_) => None,
        None => Some(v.to_string()),
    }
}

/// `-ch_layout` where `ffmpeg -h demuxer=f32le` lists it, else the older
/// `-channel_layout` (which 5.1 and later still take, as an alias).
fn layout_option(raw_help: &str) -> &'static str {
    let listed = raw_help
        .lines()
        .any(|l| l.split_whitespace().next() == Some("-ch_layout"));
    if listed {
        "-ch_layout"
    } else {
        "-channel_layout"
    }
}

/// The encoder names in `ffmpeg -encoders`.
fn parse_encoders(text: &str) -> Vec<String> {
    // The legend ends at a line of dashes.
    text.lines()
        .skip_while(|l| !l.trim_start().starts_with("---"))
        .skip(1)
        .filter_map(|l| {
            let mut w = l.split_whitespace();
            let flags = w.next()?;
            // " A....D aac   AAC (…)": six flag letters, then the name.
            (flags.len() == 6 && flags.starts_with(['A', 'V', 'S']))
                .then(|| w.next().map(str::to_string))
                .flatten()
        })
        .collect()
}

/// ffprobe's streams and format, `key=value` per line, stream after
/// stream (each begins with its `codec_name`).
fn parse_probe(text: &str) -> Option<Probe> {
    #[derive(Default)]
    struct S {
        codec: String,
        kind: String,
        bit_rate: Option<u64>,
        layout: Option<String>,
        channels: Option<usize>,
        raw_bits: Option<u32>,
        fmt: String,
        title: String,
        pic: bool,
        /// Its disposition came: every line of it but its tags is in.
        done: bool,
        index: Option<u32>,
    }
    let mut streams: Vec<S> = Vec::new();
    let mut index = None;
    let mut format_rate = None;
    for line in text.lines() {
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        match k {
            "index" => index = v.parse().ok(),
            "codec_name" => {
                streams.push(S {
                    codec: v.to_string(),
                    index: index.take(),
                    ..S::default()
                });
            }
            "codec_type" => {
                if let Some(s) = streams.last_mut() {
                    s.kind = v.to_string();
                }
            }
            "channels" => {
                if let Some(s) = streams.last_mut() {
                    s.channels = v.parse().ok().filter(|&c| c > 0);
                }
            }
            "channel_layout" => {
                if let Some(s) = streams.last_mut() {
                    s.layout = layout_name(v);
                }
            }
            "bits_per_raw_sample" => {
                if let Some(s) = streams.last_mut() {
                    s.raw_bits = v.parse().ok().filter(|&b| b > 0);
                }
            }
            "sample_fmt" => {
                if let Some(s) = streams.last_mut() {
                    s.fmt = v.to_string();
                }
            }
            // a stream's tags come after its disposition, before the
            // next stream (the format's tags are not asked for)
            "TAG:title" => {
                if let Some(s) = streams.last_mut() {
                    s.title = v.trim().to_string();
                }
            }
            "DISPOSITION:attached_pic" => {
                if let Some(s) = streams.last_mut() {
                    s.pic = v == "1";
                    s.done = true;
                }
            }
            // A stream's comes before its disposition; the format's after
            // every stream.
            "bit_rate" => match streams.last_mut() {
                Some(s) if !s.done => s.bit_rate = v.parse().ok(),
                _ => format_rate = v.parse().ok(),
            },
            _ => {}
        }
    }
    // An MPEG-TS file's programs list its streams again, ahead of the
    // streams themselves (ffprobe 5 and later): each stream once, as the
    // streams list it.
    let mut seen = Vec::new();
    streams.reverse();
    streams.retain(|s| {
        s.index.is_none_or(|i| {
            !seen.contains(&i) && {
                seen.push(i);
                true
            }
        })
    });
    streams.reverse();
    let video = streams.iter().any(|s| s.kind == "video" && !s.pic);
    let count = |k: &str| streams.iter().filter(|s| s.kind == k).count();
    let counts = Counts {
        video: count("video"),
        subtitle: count("subtitle"),
        data: count("data"),
        attachment: count("attachment"),
    };
    let (data, attachments) = (counts.data > 0, counts.attachment > 0);
    let mut sounds: Vec<Sound> = streams
        .into_iter()
        .filter(|s| s.kind == "audio")
        .map(|s| Sound {
            bits: s.raw_bits.or(match s.fmt.trim_end_matches('p') {
                "s16" => Some(16),
                "s32" => Some(32),
                _ => None,
            }),
            codec: s.codec,
            bit_rate: s.bit_rate,
            layout: s.layout,
            channels: s.channels,
            title: s.title,
        })
        .collect();
    // the whole file's rate is the sound's only when it is the one stream
    let alone = !video && sounds.len() == 1;
    let first = sounds.first_mut()?;
    if alone && first.bit_rate.is_none() {
        first.bit_rate = format_rate;
    }
    Some(Probe {
        sounds,
        video,
        data,
        attachments,
        counts,
    })
}

/// The encoder that writes `codec` (ffmpeg's codec name), where its name
/// differs from the codec's.
pub fn encoder_for(codec: &str) -> &str {
    match codec {
        "mp3" => "libmp3lame",
        "opus" => "libopus",
        "vorbis" => "libvorbis",
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_layout_without_a_name_goes_back_as_its_channels() {
        assert_eq!(layout_name("stereo").as_deref(), Some("stereo"));
        assert_eq!(layout_name("5.1(side)").as_deref(), Some("5.1(side)"));
        assert_eq!(layout_name("1 channels (FL)").as_deref(), Some("FL"));
        assert_eq!(
            layout_name("3 channels (FL+FR+LFE)").as_deref(),
            Some("FL+FR+LFE")
        );
        assert_eq!(
            layout_name("2 channels (0x1234)").as_deref(),
            Some("0x1234")
        );
        assert_eq!(layout_name("2 channels ()"), None);
        assert_eq!(layout_name("unknown"), None);
        assert_eq!(layout_name(""), None);
    }

    #[test]
    fn the_layout_option_is_the_one_this_ffmpeg_takes() {
        // ffmpeg 6.1 and 7.1
        let new = "Demuxer f32le [PCM 32-bit floating-point little-endian]:\n\
                   pcm demuxer AVOptions:\n  \
                   -sample_rate       <int>        .D......... (default 44100)\n  \
                   -channels          <int>        .D........P (default 1)\n  \
                   -ch_layout         <channel_layout> .D.........\n";
        assert_eq!(layout_option(new), "-ch_layout");
        // ffmpeg 4.4 (Ubuntu 22.04): no ch_layout
        let old = "Demuxer f32le [PCM 32-bit floating-point little-endian]:\n\
                   pcm demuxer AVOptions:\n  \
                   -sample_rate       <int>        .D......... (default 44100)\n  \
                   -channels          <int>        .D......... (default 1)\n";
        assert_eq!(layout_option(old), "-channel_layout");
        assert_eq!(
            layout_option(""),
            "-channel_layout",
            "unknown: the old name"
        );
    }

    #[test]
    fn encoders_are_read_from_the_list() {
        let text = "Encoders:\n V..... = Video\n ------\n V....D libx264  H.264\n \
                    A....D aac                  AAC (Advanced Audio Coding)\n \
                    A....D libmp3lame           libmp3lame MP3\n";
        assert_eq!(parse_encoders(text), ["libx264", "aac", "libmp3lame"]);
    }

    #[test]
    fn the_sound_stream_and_a_picture_are_found() {
        // An MP3 with a cover picture: sound only.
        let mp3 = "codec_name=mp3\ncodec_type=audio\nchannels=2\nchannel_layout=stereo\nbit_rate=192000\n\
                   DISPOSITION:attached_pic=0\ncodec_name=mjpeg\ncodec_type=video\nbit_rate=N/A\n\
                   DISPOSITION:attached_pic=1\nbit_rate=195000\n";
        let p = parse_probe(mp3).unwrap();
        assert_eq!(
            p,
            Probe {
                sounds: vec![Sound {
                    codec: "mp3".into(),
                    bit_rate: Some(192_000),
                    layout: Some("stereo".into()),
                    channels: Some(2),
                    ..Sound::default()
                }],
                video: false,
                // the cover: a picture stream, its tags kept
                counts: Counts {
                    video: 1,
                    ..Counts::default()
                },
                ..Probe::default()
            }
        );
        // A video: the sound stream's own rate, never the file's.
        let mp4 =
            "codec_name=h264\ncodec_type=video\nbit_rate=900000\nDISPOSITION:attached_pic=0\n\
                   codec_name=aac\ncodec_type=audio\nchannel_layout=mono\nbit_rate=69000\n\
                   DISPOSITION:attached_pic=0\nbit_rate=980000\n";
        let p = parse_probe(mp4).unwrap();
        assert_eq!(
            (p.sounds[0].codec.as_str(), p.sounds[0].bit_rate, p.video),
            ("aac", Some(69_000), true)
        );
        // Opus in Ogg states no stream rate: the file's.
        let opus = "codec_name=opus\ncodec_type=audio\nchannel_layout=mono\nbit_rate=N/A\n\
                    DISPOSITION:attached_pic=0\nbit_rate=64000\n";
        assert_eq!(parse_probe(opus).unwrap().sounds[0].bit_rate, Some(64_000));
        assert!(parse_probe("codec_name=h264\ncodec_type=video\n").is_none());
    }

    /// An OBS recording: a picture and a sound stream per source; every
    /// sound stream is found, in order, and a timecode is a data stream.
    #[test]
    fn every_sound_stream_is_found() {
        let obs = "codec_name=h264\ncodec_type=video\nbit_rate=N/A\nDISPOSITION:attached_pic=0\n\
                   codec_name=aac\ncodec_type=audio\nchannel_layout=stereo\nbit_rate=160000\n\
                   DISPOSITION:attached_pic=0\nTAG:title=Desktop Audio\n\
                   codec_name=opus\ncodec_type=audio\nchannel_layout=mono\nbit_rate=N/A\n\
                   DISPOSITION:attached_pic=0\nTAG:title=Mic/Aux\n\
                   codec_name=bin_data\ncodec_type=data\nbit_rate=N/A\nDISPOSITION:attached_pic=0\n\
                   bit_rate=2600000\n";
        let p = parse_probe(obs).unwrap();
        assert_eq!(
            (p.sounds[0].codec.as_str(), p.sounds[0].bit_rate),
            ("aac", Some(160_000))
        );
        assert_eq!(
            p.sounds[1],
            Sound {
                codec: "opus".into(),
                bit_rate: None,
                layout: Some("mono".into()),
                channels: None,
                bits: None,
                title: "Mic/Aux".into(),
            }
        );
        assert_eq!(p.titles(), ["Desktop Audio", "Mic/Aux"]);
        assert!(p.video && p.data && !p.attachments);
        assert_eq!(
            p.counts,
            Counts {
                video: 1,
                subtitle: 0,
                data: 1,
                attachment: 0
            }
        );
        // two sound streams of a file without a picture: the file's rate
        // is neither's
        let two = "codec_name=flac\ncodec_type=audio\nbit_rate=N/A\nDISPOSITION:attached_pic=0\n\
                   codec_name=flac\ncodec_type=audio\nsample_fmt=s32\nbits_per_raw_sample=24\n\
                   bit_rate=N/A\nDISPOSITION:attached_pic=0\nbit_rate=900000\n";
        let p = parse_probe(two).unwrap();
        assert_eq!(p.sounds[0].bit_rate, None);
        assert_eq!(p.sounds[1].bits, Some(24), "a 24-bit second track");
    }

    #[test]
    fn encoders_by_codec() {
        assert_eq!(encoder_for("mp3"), "libmp3lame");
        assert_eq!(encoder_for("aac"), "aac");
        assert_eq!(encoder_for("alac"), "alac");
    }

    /// F10: an AAC track in Matroska starts with its encoder's delay
    /// before the file's zero; ffprobe 4.4 and 6.1 say it alike.
    #[test]
    fn the_sound_starts_where_the_probe_says() {
        let text = "stream_index=1\nbest_effort_timestamp_time=-0.021000\n\
                    stream_index=1\nbest_effort_timestamp_time=0.000000\n\
                    stream_index=2\nbest_effort_timestamp_time=N/A\n\
                    index=1\nindex=2\nindex=3\nstart_time=-0.021000\n";
        assert_eq!(
            parse_starts(text),
            Some(Starts {
                file: -0.021,
                sounds: vec![Some(-0.021), None, None],
            })
        );
        assert_eq!(parse_starts("index=0\nstart_time=N/A\n"), None);
    }

    /// A file with programs (MPEG-TS) lists its streams twice: once each.
    #[test]
    fn a_programs_streams_are_counted_once() {
        let once = "index=0\ncodec_name=h264\ncodec_type=video\nDISPOSITION:attached_pic=0\n\
                    index=1\ncodec_name=aac\ncodec_type=audio\nbit_rate=128000\n\
                    DISPOSITION:attached_pic=0\n";
        let p = parse_probe(&format!("{once}{once}bit_rate=900000\n")).unwrap();
        assert_eq!(p.sounds.len(), 1);
        assert_eq!(p.counts.video, 1);
        assert_eq!(p.sounds[0].bit_rate, Some(128_000));
    }
}
