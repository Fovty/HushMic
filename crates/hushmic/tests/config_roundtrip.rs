use hushmic::config::Config;

#[test]
fn defaults_are_sane() {
    let c = Config::default();
    assert_eq!(c.model, "dpdfnet8_48khz_hr");
    assert_eq!(c.attn_limit, 100.0);
    assert!(c.enabled);
    // Repointing the SYSTEM default input is invasive and must be opt-in
    // (matches the README's documented flow).
    assert!(!c.set_default);
}

#[test]
fn sanitize_clamps_hand_edited_attn_limit() {
    // NaN would render as a literal `NaN` token in the filter-chain conf.
    let mut c = Config {
        attn_limit: f32::NAN,
        ..Config::default()
    };
    c.sanitize();
    assert_eq!(c.attn_limit, 100.0);
    c.attn_limit = -5.0;
    c.sanitize();
    assert_eq!(c.attn_limit, 0.0);
    c.attn_limit = 1e9;
    c.sanitize();
    assert_eq!(c.attn_limit, 100.0);
}

#[test]
fn toml_roundtrips() {
    let c = Config {
        enabled: false,
        mic: Some("alsa_input.x".into()),
        model: "dpdfnet2_48khz_hr".into(),
        attn_limit: 24.0,
        set_default: true,
        autostart: true,
        ..Config::default()
    };
    let s = toml::to_string_pretty(&c).unwrap();
    let back: Config = toml::from_str(&s).unwrap();
    assert_eq!(back.mic.as_deref(), Some("alsa_input.x"));
    assert_eq!(back.attn_limit, 24.0);
    assert!(back.set_default && back.autostart && !back.enabled);
}

#[cfg(feature = "speakers")]
#[test]
fn the_speakers_section_appears_only_once_used() {
    use hushmic::config::SpeakersConfig;
    // An untouched config has no [speakers] section, and reading one back
    // without it gives the defaults: off, the system default, Maximum.
    let plain = toml::to_string_pretty(&Config::default()).unwrap();
    assert!(!plain.contains("speakers"), "{plain}");
    let back: Config = toml::from_str(&plain).unwrap();
    assert_eq!(back.speakers, None);
    assert_eq!(back.speakers(), SpeakersConfig::default());
    assert!(!back.speakers().enabled);
    assert_eq!(back.speakers().output, None);
    assert_eq!(back.speakers().attn_limit, 100.0);

    // Once used it round-trips, after the top-level keys.
    let mut c = Config::default();
    c.speakers_mut().enabled = true;
    c.speakers_mut().output = Some("alsa_output.usb-dac".into());
    c.speakers_mut().attn_limit = 12.0;
    let s = toml::to_string_pretty(&c).unwrap();
    assert!(s.contains("[speakers]"), "{s}");
    let back: Config = toml::from_str(&s).unwrap();
    assert_eq!(back.speakers, c.speakers);
    // A system-default output writes no key.
    c.speakers_mut().output = None;
    let s = toml::to_string_pretty(&c).unwrap();
    assert!(!s.contains("output"), "{s}");

    // A partial hand-written section takes the rest from the defaults, and
    // sanitize clamps it like the globals.
    let mut hand: Config =
        toml::from_str("[speakers]\nattn_limit = 1e9\noutput = \"  \"\n").unwrap();
    hand.sanitize();
    let s = hand.speakers();
    assert!(!s.enabled);
    assert_eq!(s.attn_limit, 100.0);
    assert_eq!(s.output, None);
}

/// A build without HushMic Speakers keeps a `[speakers]` section a build
/// with it wrote: the file loads, and a save writes the section back as it
/// was (a value its own build would clamp too).
#[cfg(not(feature = "speakers"))]
#[test]
fn a_speakers_section_is_kept_as_it_is() {
    let section =
        "[speakers]\nenabled = true\noutput = \"alsa_output.usb-dac\"\nattn_limit = 250.0\n";
    let mut c: Config = toml::from_str(&format!("attn_limit = 24.0\n\n{section}")).unwrap();
    c.sanitize();
    c.attn_limit = 6.0;
    let s = toml::to_string_pretty(&c).unwrap();
    assert!(s.ends_with(section), "{s}");
}
