//! What counts as a recording before anything is opened: by its name's
//! extension (the Files page's icon at once, what a drop takes, the file
//! chooser's filter) and by its first bytes (a drop of something that only
//! looks like one). Opening it is the real test ([`super::worker`]); this
//! is what can be said in no time.

use std::io::Read;
use std::path::Path;

/// Extensions of sound files HushMic (or ffmpeg) opens.
pub const AUDIO: &[&str] = &[
    "wav", "wave", "w64", "rf64", "bwf", "mp3", "mp2", "m4a", "m4b", "aac", "adts", "flac", "ogg",
    "oga", "opus", "spx", "wma", "aif", "aiff", "aifc", "caf", "alac", "ape", "wv", "mka", "amr",
    "3ga", "ac3", "eac3", "dts", "au", "snd", "weba", "mpc", "tta",
];

/// Extensions of video files: their sound is cleaned, the picture kept.
pub const VIDEO: &[&str] = &[
    "mp4", "m4v", "mkv", "webm", "mov", "qt", "avi", "wmv", "asf", "flv", "f4v", "mpg", "mpeg",
    "m2v", "vob", "mts", "m2ts", "ts", "3gp", "3g2", "ogv", "mxf", "divx", "rm", "rmvb", "dv",
];

/// The concrete MIME types of those (a portal's chooser that can't match
/// `audio/*` still matches these; the globs catch the rest).
pub const MIME: &[&str] = &[
    "audio/wav",
    "audio/x-wav",
    "audio/vnd.wave",
    "audio/mpeg",
    "audio/mp4",
    "audio/x-m4a",
    "audio/aac",
    "audio/x-aac",
    "audio/flac",
    "audio/x-flac",
    "audio/ogg",
    "audio/x-vorbis+ogg",
    "audio/x-opus+ogg",
    "audio/opus",
    "audio/x-ms-wma",
    "audio/aiff",
    "audio/x-aiff",
    "audio/x-caf",
    "audio/x-matroska",
    "audio/webm",
    "audio/amr",
    "audio/ac3",
    "audio/x-ape",
    "audio/x-wavpack",
    "audio/basic",
    "video/mp4",
    "video/x-m4v",
    "video/x-matroska",
    "video/webm",
    "video/quicktime",
    "video/x-msvideo",
    "video/x-ms-wmv",
    "video/x-ms-asf",
    "video/x-flv",
    "video/mpeg",
    "video/mp2t",
    "video/3gpp",
    "video/3gpp2",
    "video/ogg",
    "video/x-ogm+ogg",
    "video/x-theora+ogg",
];

/// What a file's name says it is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Named {
    Audio,
    Video,
    /// No extension of a recording (a text file, an image, none at all).
    Other,
}

/// What `path`'s extension says (case does not matter: phones write
/// `.MP4`).
pub fn named(path: &Path) -> Named {
    let Some(ext) = path.extension().map(|e| e.to_string_lossy().to_lowercase()) else {
        return Named::Other;
    };
    if VIDEO.contains(&ext.as_str()) {
        Named::Video
    } else if AUDIO.contains(&ext.as_str()) {
        Named::Audio
    } else {
        Named::Other
    }
}

/// The chooser's glob patterns: every extension, in lower and upper case
/// (a GTK chooser matches globs by case).
pub fn globs() -> Vec<String> {
    AUDIO
        .iter()
        .chain(VIDEO)
        .flat_map(|e| [format!("*.{e}"), format!("*.{}", e.to_uppercase())])
        .collect()
}

/// What a file's first bytes say.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sniffed {
    /// A media container's or stream's signature.
    Media,
    /// Plain text (a note, a playlist, a subtitle file).
    Text,
    /// Something else that is surely not sound: an image, an archive, a
    /// document.
    NotMedia,
    /// Nothing to go by (unknown bytes, too short, unreadable): opening
    /// it decides.
    Unknown,
}

/// Look at `path`'s first bytes (4 KiB at most).
pub fn sniff(path: &Path) -> Sniffed {
    let mut head = Vec::with_capacity(4096);
    let read = std::fs::File::open(path).and_then(|f| f.take(4096).read_to_end(&mut head));
    match read {
        Ok(_) => sniff_bytes(&head),
        Err(_) => Sniffed::Unknown,
    }
}

/// [`sniff`] on bytes already read.
pub fn sniff_bytes(b: &[u8]) -> Sniffed {
    let at = |i: usize, s: &[u8]| b.len() >= i + s.len() && &b[i..i + s.len()] == s;
    let media = at(0, b"RIFF") && (at(8, b"WAVE") || at(8, b"AVI "))
        || at(0, b"RF64")
        || at(0, b"BW64")
        || at(0, b"FORM") && (at(8, b"AIFF") || at(8, b"AIFC"))
        || at(0, b"fLaC")
        || at(0, b"OggS")
        || at(0, b"ID3")
        || at(4, b"ftyp")
        || at(4, b"moov")
        || at(4, b"mdat")
        || at(4, b"wide")
        || at(4, b"free")
        || at(0, &[0x1a, 0x45, 0xdf, 0xa3])
        || at(0, &[0x30, 0x26, 0xb2, 0x75, 0x8e, 0x66, 0xcf, 0x11])
        || at(0, b"FLV")
        || at(0, b"caff")
        || at(0, b"MAC ")
        || at(0, b"wvpk")
        || at(0, b"TTA1")
        || at(0, b"MPCK")
        || at(0, b"MP+")
        // DTS: the core's sync word, big- and little-endian
        || at(0, &[0x7f, 0xfe, 0x80, 0x01])
        || at(0, &[0xfe, 0x7f, 0x01, 0x80])
        || at(0, b"#!AMR")
        || at(0, b".snd")
        || at(0, b".RMF")
        || at(0, &[0x00, 0x00, 0x01, 0xba])
        || at(0, &[0x00, 0x00, 0x01, 0xb3])
        || at(0, &[0x0b, 0x77])
        || mpeg_ts(b)
        || mpeg_audio(b);
    if media {
        return Sniffed::Media;
    }
    let not_media = at(0, b"%PDF")
        || at(0, &[0x89, b'P', b'N', b'G'])
        || at(0, &[0xff, 0xd8, 0xff])
        || at(0, b"GIF8")
        || at(0, b"PK\x03\x04")
        || at(0, &[0x1f, 0x8b])
        || at(0, b"7z\xbc\xaf")
        || at(0, b"Rar!")
        || at(0, &[0x7f, b'E', b'L', b'F'])
        || at(0, b"BZh")
        || at(0, &[0xfd, b'7', b'z', b'X', b'Z'])
        || at(0, &[0xd0, 0xcf, 0x11, 0xe0])
        || at(0, b"SQLite format 3");
    if not_media {
        return Sniffed::NotMedia;
    }
    if !b.is_empty() && text(b) {
        return Sniffed::Text;
    }
    Sniffed::Unknown
}

/// An MPEG transport stream: a sync byte every 188 bytes.
fn mpeg_ts(b: &[u8]) -> bool {
    b.len() >= 188 * 3 && (0..3).all(|k| b[k * 188] == 0x47)
}

/// An MPEG audio frame header (or ADTS) at the start: 11 sync bits, a
/// layer, a bitrate and a rate that are not reserved.
fn mpeg_audio(b: &[u8]) -> bool {
    if b.len() < 4 || b[0] != 0xff || b[1] & 0xe0 != 0xe0 {
        return false;
    }
    // ADTS (AAC): 1111 1111 1111 x00x
    if b[1] & 0xf6 == 0xf0 {
        return true;
    }
    let layer = (b[1] >> 1) & 3;
    let bitrate = b[2] >> 4;
    let rate = (b[2] >> 2) & 3;
    layer != 0 && bitrate != 0 && bitrate != 15 && rate != 3
}

/// Printable UTF-8 with line breaks: text.
fn text(b: &[u8]) -> bool {
    // a cut at the 4096th byte may split a character
    let s = match std::str::from_utf8(b) {
        Ok(s) => s,
        Err(e) if e.valid_up_to() + 4 >= b.len() => {
            std::str::from_utf8(&b[..e.valid_up_to()]).unwrap_or("")
        }
        Err(_) => return false,
    };
    !s.is_empty()
        && s.chars()
            .all(|c| !c.is_control() || matches!(c, '\n' | '\r' | '\t' | '\u{feff}'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_name_says_audio_video_or_neither() {
        assert_eq!(named(Path::new("/a/talk.M4A")), Named::Audio);
        assert_eq!(named(Path::new("/a/VID_0001.MP4")), Named::Video);
        assert_eq!(named(Path::new("/a/notes.txt")), Named::Other);
        assert_eq!(named(Path::new("/a/README")), Named::Other);
        assert!(globs().contains(&"*.MKV".to_string()));
        assert!(globs().contains(&"*.opus".to_string()));
        for m in MIME {
            assert!(m.starts_with("audio/") || m.starts_with("video/"), "{m}");
        }
    }

    #[test]
    fn the_first_bytes_tell_media_from_text_and_documents() {
        let mut wav = b"RIFF\0\0\0\0WAVEfmt ".to_vec();
        wav.resize(64, 0);
        assert_eq!(sniff_bytes(&wav), Sniffed::Media);
        assert_eq!(sniff_bytes(b"\0\0\0\x20ftypisom\0\0"), Sniffed::Media);
        assert_eq!(sniff_bytes(b"ID3\x04\0\0\0\0\0\0"), Sniffed::Media);
        assert_eq!(sniff_bytes(&[0xff, 0xfb, 0x90, 0x64]), Sniffed::Media);
        assert_eq!(sniff_bytes(&[0x1a, 0x45, 0xdf, 0xa3, 1]), Sniffed::Media);
        assert_eq!(sniff_bytes(b"OggS\0\x02"), Sniffed::Media);
        for head in [
            &b"TTA1\x01\0"[..],
            b"MPCK\0",
            b"MP+\x07",
            &[0x7f, 0xfe, 0x80, 0x01, 0],
        ] {
            assert_eq!(sniff_bytes(head), Sniffed::Media, "{head:?}");
        }
        assert_eq!(sniff_bytes(b"Meeting notes\nTuesday\n"), Sniffed::Text);
        assert_eq!(sniff_bytes(b"%PDF-1.7"), Sniffed::NotMedia);
        assert_eq!(
            sniff_bytes(&[0x89, b'P', b'N', b'G', 0x0d]),
            Sniffed::NotMedia
        );
        assert_eq!(sniff_bytes(b"PK\x03\x04zip"), Sniffed::NotMedia);
        // random bytes: opening decides
        assert_eq!(
            sniff_bytes(&[0x13, 0x88, 0xa0, 0x02, 0xfe]),
            Sniffed::Unknown
        );
        assert_eq!(sniff_bytes(b""), Sniffed::Unknown);
    }
}
