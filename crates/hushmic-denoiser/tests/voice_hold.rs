//! The voice hold on the real models: what it must leave alone.
//!
//! That it keeps a far voice out is a property of the models on voices
//! with a room around them; it was measured on recordings (HushMic issue
//! 34) and is not something a short fixture can pin. What is pinned here
//! is the safe side: on, nothing changes while speech is being passed, and
//! putting the memory back makes no sound in a pause.

mod common;

use common::{dev_denoiser, repo_root};
use hushmic_denoiser::{Denoiser, HOP};

fn read_flac_mono_f32(p: &std::path::Path) -> Vec<f32> {
    let mut r = claxon::FlacReader::open(p).expect("open flac");
    r.samples()
        .map(|s| s.expect("flac sample") as f32 / 32768.0)
        .collect()
}

fn run(den: &mut Denoiser, input: &[f32]) -> Vec<f32> {
    let mut out = Vec::with_capacity(input.len());
    for hop in input.as_chunks::<HOP>().0 {
        let mut o = [0f32; HOP];
        den.process_hop(hop, &mut o).expect("inference");
        out.extend_from_slice(&o);
    }
    out
}

fn peak(x: &[f32]) -> f32 {
    x.iter().fold(0f32, |m, v| m.max(v.abs()))
}

#[test]
fn on_it_changes_no_speech_and_makes_no_sound_in_a_pause() {
    let noisy = read_flac_mono_f32(&repo_root().join("tests/fixtures/noisy_public_48k.flac"));
    // six seconds of speech in noise, then five seconds of nothing: the
    // memory goes back at two and at four seconds of the pause
    let speech = 600 * HOP;
    let mut input = noisy[..speech].to_vec();
    input.resize(speech + 500 * HOP, 0.0);
    for model in ["dpdfnet8_48khz_hr.onnx", "dpdfnet2_48khz_hr.onnx"] {
        let (Some(mut plain), Some(mut held)) = (dev_denoiser(model), dev_denoiser(model)) else {
            eprintln!("skipping the voice hold test: {model} not provisioned");
            continue;
        };
        assert!(!held.voice_hold(), "off unless turned on");
        held.set_voice_hold(true);
        assert!(held.voice_hold());
        let (a, b) = (run(&mut plain, &input), run(&mut held, &input));
        assert!(peak(&a[..speech]) > 0.05, "{model}: the fixture has speech");
        // the pause begins before anything was put back
        let untouched = speech + 190 * HOP;
        assert!(a[..untouched] == b[..untouched], "{model}: speech changed");
        // one second in, the model's own tail is over; from there on the
        // pause is silent, through both moments the memory goes back
        let pause = &b[speech + 100 * HOP..];
        assert!(peak(pause) < 1e-4, "{model}: {} in the pause", peak(pause));
        // off again, and from a fresh start, it is the plain denoiser
        held.set_voice_hold(false);
        held.reset();
        plain.reset();
        assert!(
            run(&mut plain, &input) == run(&mut held, &input),
            "{model}: turned off, it still differs"
        );
    }
}
