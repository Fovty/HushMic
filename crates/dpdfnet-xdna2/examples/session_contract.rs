//! Run against an isolated session bus with the development service already started.
use dpdfnet_xdna2::{file_hash, policy, set_policy, Stream, AUTO, CPU, PREFER_NPU, WORDS};
use std::{path::Path, thread, time::Duration};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let model = std::env::args()
        .nth(1)
        .ok_or("provide the original ONNX model")?;
    let hash = file_hash(Path::new(&model))?;
    set_policy(CPU)?;
    assert_eq!(policy()?.0, CPU);
    assert!(Stream::open(&hash).is_err());
    set_policy(AUTO)?;
    assert!(!policy()?.2);
    assert!(Stream::open(&hash).is_err());
    set_policy(PREFER_NPU)?;
    assert!(Stream::open("unrelated").is_err());
    let mut stream = Stream::open(&hash)?;
    assert!(Stream::open(&hash).is_err());
    let mut input = [0.0; WORDS];
    input[0] = 0.1;
    input[112] = 0.3;
    let mut first = [0.0_f32; WORDS];
    stream.step(&input, &mut first)?;
    for _ in 0..12 {
        stream.step(&input, &mut [0.0; WORDS])?;
    }
    stream.reset()?;
    let mut replay = [0.0_f32; WORDS];
    stream.step(&input, &mut replay)?;
    assert!(first
        .iter()
        .zip(replay)
        .all(|(a, b)| a.to_bits() == b.to_bits()));
    drop(stream);
    thread::sleep(Duration::from_millis(200));
    drop(Stream::open(&hash)?);
    set_policy(CPU)?;
    println!("CPU, Auto, model identity, exclusive lease, 14 frames, reset and reopen verified");
    Ok(())
}
