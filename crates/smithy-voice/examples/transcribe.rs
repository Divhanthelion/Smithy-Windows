//! Transcribe WAV files with the same engine the microphone uses, and time it.
//!
//! cargo run --release -p smithy-voice --example transcribe -- a.wav [b.wav …]
//!
//! With no files, it transcribes the test recordings that ship with the model.
//! The first run downloads the model (464 MB), as the first press of the
//! microphone would.

use smithy_voice::inference::{Engine, SAMPLE_RATE};
use smithy_voice::ModelConfig;
use std::time::Instant;

fn main() -> anyhow::Result<()> {
    let config = ModelConfig::default();
    let started = Instant::now();
    let engine = Engine::load(&config)?;
    println!("loaded in {:.1}s", started.elapsed().as_secs_f32());

    let mut files: Vec<String> = std::env::args().skip(1).collect();
    if files.is_empty() {
        let tests = config.dir().join("test_wavs");
        for entry in std::fs::read_dir(&tests)? {
            let path = entry?.path();
            if path.extension().is_some_and(|e| e == "wav") {
                files.push(path.to_string_lossy().into_owned());
            }
        }
        files.sort();
    }
    for file in files {
        let wave = sherpa_onnx::Wave::read(&file)
            .ok_or_else(|| anyhow::anyhow!("could not read {file}"))?;
        let samples = if wave.sample_rate() == SAMPLE_RATE {
            wave.samples().to_vec()
        } else {
            println!(
                "{file}: {} Hz, skipped (the example only feeds 16 kHz)",
                wave.sample_rate()
            );
            continue;
        };
        let seconds = samples.len() as f32 / SAMPLE_RATE as f32;
        let t = Instant::now();
        let text = engine.transcribe(&samples);
        let took = t.elapsed().as_secs_f32();
        println!(
            "{file}: {seconds:.1}s of audio in {took:.2}s ({:.0}x real time)\n  {text}",
            seconds / took.max(0.001)
        );
    }
    Ok(())
}
