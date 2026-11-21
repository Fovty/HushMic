//! Exercise the negotiated PCM worker from a separate process.
use dpdfnet_xdna2::{
    pcm::{Stream, HOP},
    session_busy,
};
use std::{
    env, fs,
    io::{self, BufWriter, Write},
    path::Path,
    thread,
    time::{Duration, Instant},
};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

// FP32 encoding of the offline fixture's 10^(-30/20).
const CAP: f32 = f32::from_bits(1023510242);
const LIMITS: [f32; 19] = [
    CAP, 1.0, 0.0, CAP, 0.0, CAP, CAP, 1.0, 0.0, 1e-6, CAP, 0.0, CAP, CAP, 1.0, 0.0, 0.0, CAP, CAP,
];

fn open(model: &str, after_close: bool) -> Result<Stream> {
    let until = Instant::now() + Duration::from_secs(3);
    loop {
        match Stream::open(model) {
            Ok(stream) => return Ok(stream),
            Err(error) if after_close && session_busy(&error) && Instant::now() < until => {
                thread::sleep(Duration::from_millis(10));
            }
            Err(error) => return Err(error.into()),
        }
    }
}

fn read_pcm(path: &Path) -> Result<Vec<f32>> {
    let raw = fs::read(path)?;
    if raw.is_empty() || raw.len() % (4 * HOP) != 0 {
        return Err(io::Error::from(io::ErrorKind::InvalidData).into());
    }
    Ok(raw
        .chunks_exact(4)
        .map(|word| f32::from_le_bytes(word.try_into().unwrap()))
        .collect())
}

fn quality(stream: &mut Stream, pcm: &[f32], out: &Path) -> Result<()> {
    if pcm.len() < LIMITS.len() * HOP {
        return Err("Incomplete PCM reset fixture".into());
    }
    let mut output = BufWriter::new(fs::File::create(out)?);
    let mut target = 0.0f32;
    let mut result = [0.0; HOP];
    for epoch in 0..6 {
        let mut mix = 0.0f32;
        if epoch != 0 {
            // Exercise restart after the client has left the worker idle.
            thread::sleep(Duration::from_millis(30));
            stream.reset()?;
        }
        for (index, limit) in LIMITS.iter().enumerate() {
            match index {
                1 | 10 | 18 => target = 1.0,
                6 | 16 => target = 0.0,
                _ => (),
            }
            mix += (target - mix).clamp(-0.25, 0.25);
            let audio = &pcm[index * HOP..(index + 1) * HOP];
            let input_cut = (epoch * 113 + index * 17) % (HOP + 1);
            let output_cut = HOP - input_cut;
            stream.step_parts(
                audio.split_at(input_cut),
                result.split_at_mut(output_cut),
                *limit,
                mix,
            )?;
            for value in result {
                output.write_all(&value.to_le_bytes())?;
            }
        }
    }
    output.flush()?;
    Ok(())
}

fn timing(stream: &mut Stream, pcm: &[f32], out: &Path, paced: bool, mix: bool) -> Result<()> {
    let mut result = [0.0; HOP];
    let mut entries = Vec::with_capacity(160);
    let period = Duration::from_millis(10);
    let (limit, bypass) = if mix { (CAP, 0.5) } else { (0.0, 0.0) };
    let mut planned = Instant::now();
    for index in 0..200 {
        if paced {
            planned += period;
            thread::sleep(planned.saturating_duration_since(Instant::now()));
        } else {
            planned = Instant::now();
        }
        let start = Instant::now();
        let offset = (index * HOP) % pcm.len();
        stream.step(
            pcm[offset..offset + HOP].try_into()?,
            &mut result,
            limit,
            bypass,
        )?;
        let end = Instant::now();
        if index >= 40 {
            entries.push((
                start.duration_since(planned),
                end.duration_since(start),
                end.duration_since(planned),
            ));
        }
        if paced {
            // Match the native diagnostic clock's explicit rebasing rule.
            planned = planned.max(end - period);
        }
    }
    let mut writer = BufWriter::new(fs::File::create(out)?);
    for (entry, body, response) in entries {
        writeln!(
            writer,
            "{} {} {}",
            entry.as_nanos(),
            body.as_nanos(),
            response.as_nanos()
        )?;
    }
    writer.flush()?;
    Ok(())
}

fn checkpoint(label: &str) -> Result<()> {
    println!("{label}");
    io::stdout().flush()?;
    let mut ack = String::new();
    io::stdin().read_line(&mut ack)?;
    if ack.trim() != "ack" {
        return Err("Missing PCM measurement acknowledgement".into());
    }
    Ok(())
}

fn main() -> Result<()> {
    let args: Vec<_> = env::args().skip(1).collect();
    let [operation, model, input, output] = args.as_slice() else {
        return Err("usage: pcm_probe OP MODEL_HASH INPUT OUTPUT".into());
    };
    let pcm = read_pcm(Path::new(input))?;
    let out = Path::new(output);
    let mut stream = open(model, true)?;
    match operation.as_str() {
        "quality" => {
            assert!(matches!(Stream::open(model), Err(error) if session_busy(&error)));
            checkpoint("OPEN")?;
            quality(&mut stream, &pcm, out)?;
            checkpoint("COMPLETE")?;
        }
        "paced" | "continuous" | "paced-nomix" | "continuous-nomix" => {
            checkpoint("OPEN")?;
            timing(
                &mut stream,
                &pcm,
                out,
                operation.starts_with("paced"),
                !operation.ends_with("-nomix"),
            )?;
            checkpoint("COMPLETE")?;
        }
        "malformed" => {
            let mut malformed = [0.0; HOP];
            malformed[109] = f32::NAN;
            let mut response = [0.0; HOP];
            assert!(stream.step(&malformed, &mut response, CAP, 0.5).is_err());
            assert_eq!(
                stream.reset().unwrap_err().kind(),
                io::ErrorKind::BrokenPipe
            );
            let mut next = open(model, true)?;
            next.step(pcm[..HOP].try_into()?, &mut response, CAP, 0.5)?;
        }
        "fault" | "fault-nomix" => {
            let mut response = [0.0; HOP];
            let (limit, bypass) = if operation == "fault" {
                (CAP, 0.5)
            } else {
                (0.0, 0.0)
            };
            for index in 0..6 {
                let result = stream.step(
                    pcm[index * HOP..(index + 1) * HOP].try_into()?,
                    &mut response,
                    limit,
                    bypass,
                );
                assert_eq!(result.is_ok(), index < 5, "submit {index}");
            }
            assert_eq!(
                stream.reset().unwrap_err().kind(),
                io::ErrorKind::BrokenPipe
            );
        }
        _ => return Err("Unknown PCM probe operation".into()),
    }
    Ok(())
}
