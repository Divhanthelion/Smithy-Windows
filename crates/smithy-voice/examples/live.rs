//! Live dictation without a microphone: a WAV file is fed through the same
//! path the microphone uses, block by block at real-time pace, followed by
//! silence, and every event is printed with its time.
//!
//! cargo run --release -p smithy-voice --example live -- [file.wav]
//!
//! Shows the words arriving while "speech" is still coming in, and whether a
//! pause ends the dictation by itself.

use smithy_voice::inference::{Event, Transcriber, SAMPLE_RATE};
use smithy_voice::ModelConfig;
use std::time::{Duration, Instant};

fn main() -> anyhow::Result<()> {
    let config = ModelConfig::default();
    let file = std::env::args().nth(1).unwrap_or_else(|| {
        config
            .dir()
            .join("test_wavs")
            .join("0.wav")
            .to_string_lossy()
            .into_owned()
    });
    let wave =
        sherpa_onnx::Wave::read(&file).ok_or_else(|| anyhow::anyhow!("could not read {file}"))?;
    anyhow::ensure!(wave.sample_rate() == SAMPLE_RATE, "feed a 16 kHz file");

    let (tx, rx) = crossbeam_channel::unbounded::<Event>();
    let transcriber = Transcriber::new(tx);
    transcriber.load(config);
    match rx.recv()? {
        Event::Loaded => {}
        other => anyhow::bail!("load: {other:?}"),
    }

    let start = Instant::now();
    let mut sink = transcriber.begin(SAMPLE_RATE as u32);
    let block = SAMPLE_RATE as usize / 10; // 100 ms, about what a microphone delivers
    let speech = wave.samples().to_vec();
    let silence = vec![0.0f32; SAMPLE_RATE as usize * 3];
    let speech_ends = speech.len() as f32 / SAMPLE_RATE as f32;
    let audio: Vec<f32> = speech.into_iter().chain(silence).collect();

    let mut ended = None;
    for chunk in audio.chunks(block) {
        sink(chunk);
        std::thread::sleep(Duration::from_millis(100));
        while let Ok(event) = rx.try_recv() {
            let t = start.elapsed().as_secs_f32();
            match event {
                Event::Partial(text) => println!("{t:5.2}s  … {text}"),
                Event::Transcribed(text) => {
                    println!("{t:5.2}s  ✓ {text}");
                    ended = Some(t);
                }
                other => println!("{t:5.2}s  {other:?}"),
            }
        }
        if ended.is_some() {
            break;
        }
    }
    match ended {
        Some(t) => println!(
            "speech ended at {speech_ends:.2}s; the pause closed the dictation at {t:.2}s ({:.2}s later)",
            t - speech_ends
        ),
        None => println!("no pause ended the dictation within 3 s of silence"),
    }
    Ok(())
}
