//! Denoise a WAV file: `cargo run --release --example denoise_wav -- \
//! dpdfnet8_48khz_hr.onnx noisy.wav cleaned.wav`
//!
//! Input must be 48 kHz mono (the only format the models run at). The cleaned
//! file is written as 16-bit PCM, sample-aligned with the input: the stream's
//! latency tail is drained with `pending()` zeros and the output truncated to
//! the input length.

use hushmic_denoiser::{Denoiser, StreamDenoiser, SAMPLE_RATE};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    let [_, model, input, output] = &args[..] else {
        eprintln!("usage: denoise_wav <model.onnx> <in.wav> <out.wav>");
        std::process::exit(2);
    };

    let mut reader = hound::WavReader::open(input)?;
    let spec = reader.spec();
    if spec.sample_rate != SAMPLE_RATE || spec.channels != 1 {
        return Err(format!(
            "input must be {SAMPLE_RATE} Hz mono, got {} Hz / {} ch",
            spec.sample_rate, spec.channels
        )
        .into());
    }
    let samples: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Float => reader.samples::<f32>().collect::<Result<_, _>>()?,
        hound::SampleFormat::Int => {
            let max = (1i64 << (spec.bits_per_sample - 1)) as f32;
            reader
                .samples::<i32>()
                .map(|s| s.map(|v| v as f32 / max))
                .collect::<Result<_, _>>()?
        }
    };

    let mut stream = StreamDenoiser::new(Denoiser::from_file(model)?);
    let mut cleaned = Vec::with_capacity(samples.len());
    for chunk in samples.chunks(4096) {
        cleaned.extend_from_slice(stream.process(chunk));
    }
    // drain the latency tail, then align 1:1 with the input
    cleaned.extend_from_slice(stream.process(&vec![0f32; stream.pending()]));
    let latency = stream.denoiser().latency_samples();
    let cleaned = &cleaned[latency..latency + samples.len()];

    let mut writer = hound::WavWriter::create(
        output,
        hound::WavSpec {
            channels: 1,
            sample_rate: SAMPLE_RATE,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        },
    )?;
    for &s in cleaned {
        writer.write_sample((s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16)?;
    }
    writer.finalize()?;
    if let Some(e) = stream.take_error() {
        eprintln!("note: some frames degraded to near-silence: {e}");
    }
    println!("wrote {} ({} samples)", output, samples.len());
    Ok(())
}
