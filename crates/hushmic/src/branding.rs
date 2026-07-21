//! Shared brand assets: the embedded product logo and its decode into an
//! egui window icon. Used by every eframe window (A/B test, About) so the
//! binary stays self-contained — no icon-theme lookup at runtime.

// The packaging icon used everywhere else (desktop file, .deb hicolor
// install), embedded at compile time.
const ICON_PNG: &[u8] = include_bytes!("../../../packaging/hushmic-256.png");

/// Decode an embedded 8-bit RGBA PNG. Any decode surprise yields None.
fn decode_rgba(bytes: &[u8]) -> Option<(u32, u32, Vec<u8>)> {
    let decoder = png::Decoder::new(std::io::Cursor::new(bytes));
    let mut reader = decoder.read_info().ok()?;
    let mut rgba = vec![0; reader.output_buffer_size()?];
    let info = reader.next_frame(&mut rgba).ok()?;
    if info.color_type != png::ColorType::Rgba || info.bit_depth != png::BitDepth::Eight {
        return None;
    }
    rgba.truncate(info.buffer_size());
    Some((info.width, info.height, rgba))
}

/// Decode the embedded icon PNG into viewport `IconData`. Any decode
/// surprise yields None — the window simply opens without an icon.
pub fn window_icon() -> Option<eframe::egui::IconData> {
    let (width, height, rgba) = decode_rgba(ICON_PNG)?;
    Some(eframe::egui::IconData {
        rgba,
        width,
        height,
    })
}

// The tray status icons at the two sizes SNI hosts actually render,
// embedded so the tray still shows a face when the hicolor ladder is not
// installed (raw cargo-build runs) or the host ignores IconThemePath.
macro_rules! tray_png {
    ($size:literal, $name:literal) => {
        include_bytes!(concat!(
            "../../../packaging/tray/hicolor/",
            $size,
            "/status/",
            $name,
            ".png"
        ))
        .as_slice()
    };
}

/// RGBA pixmaps (22 px and 48 px) for a tray status icon name; empty for
/// unknown names or decode surprises.
pub fn tray_icon_rgba(name: &str) -> Vec<(u32, u32, Vec<u8>)> {
    let sources: [&[u8]; 2] = match name {
        "hushmic-tray" => [
            tray_png!("22x22", "hushmic-tray"),
            tray_png!("48x48", "hushmic-tray"),
        ],
        "hushmic-tray-off" => [
            tray_png!("22x22", "hushmic-tray-off"),
            tray_png!("48x48", "hushmic-tray-off"),
        ],
        "hushmic-tray-bypass" => [
            tray_png!("22x22", "hushmic-tray-bypass"),
            tray_png!("48x48", "hushmic-tray-bypass"),
        ],
        "hushmic-tray-mute" => [
            tray_png!("22x22", "hushmic-tray-mute"),
            tray_png!("48x48", "hushmic-tray-mute"),
        ],
        "hushmic-tray-error" => [
            tray_png!("22x22", "hushmic-tray-error"),
            tray_png!("48x48", "hushmic-tray-error"),
        ],
        _ => return Vec::new(),
    };
    sources.iter().filter_map(|b| decode_rgba(b)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_window_icon_decodes_to_256_rgba() {
        let icon = window_icon().expect("embedded packaging icon must decode");
        assert_eq!((icon.width, icon.height), (256, 256));
        assert_eq!(icon.rgba.len(), 256 * 256 * 4);
    }

    #[test]
    fn tray_pixmaps_decode_at_both_sizes_for_every_status() {
        for name in [
            "hushmic-tray",
            "hushmic-tray-off",
            "hushmic-tray-bypass",
            "hushmic-tray-mute",
            "hushmic-tray-error",
        ] {
            let pix = tray_icon_rgba(name);
            assert_eq!(pix.len(), 2, "{name}");
            for (w, h, rgba) in &pix {
                assert!(matches!(*w, 22 | 48), "{name}: {w}");
                assert_eq!(w, h);
                assert_eq!(rgba.len(), (*w as usize) * (*h as usize) * 4);
                // A real glyph, not a blank canvas.
                assert!(rgba.chunks_exact(4).any(|px| px[3] > 200), "{name}");
            }
        }
        assert!(tray_icon_rgba("no-such-icon").is_empty());
    }
}
