use anyhow::{anyhow, Result};
use sherpa_onnx::{OnlineRecognizer, OnlineRecognizerConfig};
use std::sync::mpsc;

use crate::model::ModelConfig;
use crate::voice_debug;

/// The rate the recognizer is fed at.
pub const SAMPLE_RATE: i32 = 16_000;

/// What the supervisor is told to do.
pub enum Command {
    Load(ModelConfig),
    /// PCM, f32, 16 kHz, mono.
    Transcribe(Vec<f32>),
    Shutdown,
}

/// What comes back.
///
/// Events on a channel rather than replies, because the caller here is a UI
/// thread that cannot await anything. This is the shape the rest of Smithy
/// already uses to get work off a thread and onto the screen —
/// `app_state::bridge` ticks a signal and the effect drains the queue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    Loaded,
    Transcribed(String),
    /// Loading or transcription failed, with a sentence saying which and why.
    Failed(String),
}

/// A handle to the thread the recognizer runs on.
///
/// **A dedicated OS thread, not a task.** Decoding is solid compute with no
/// await points in it; on any shared executor it would block whatever else was
/// scheduled there, and on the UI thread it would freeze the window.
pub struct Transcriber {
    tx: mpsc::Sender<Command>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Transcriber {
    /// Spawn the supervisor. Nothing is loaded until [`Self::load`] is called.
    pub fn new(events: crossbeam_channel::Sender<Event>) -> Self {
        let (tx, rx) = mpsc::channel::<Command>();

        let thread = std::thread::Builder::new()
            .name("smithy-voice".into())
            .spawn(move || Self::run_loop(rx, events))
            .expect("failed to spawn the voice thread");

        Self {
            tx,
            thread: Some(thread),
        }
    }

    /// Fetch and load the model. Answers with [`Event::Loaded`] or
    /// [`Event::Failed`].
    pub fn load(&self, config: ModelConfig) {
        let _ = self.tx.send(Command::Load(config));
    }

    /// Transcribe captured audio. Answers with [`Event::Transcribed`].
    pub fn transcribe(&self, audio_f32_16khz: Vec<f32>) {
        let _ = self.tx.send(Command::Transcribe(audio_f32_16khz));
    }

    fn run_loop(rx: mpsc::Receiver<Command>, events: crossbeam_channel::Sender<Event>) {
        let mut engine: Option<Engine> = None;

        while let Ok(command) = rx.recv() {
            match command {
                Command::Load(config) => {
                    voice_debug!("loading {} …", config.name);
                    match Engine::load(&config) {
                        Ok(loaded) => {
                            voice_debug!("model ready");
                            engine = Some(loaded);
                            let _ = events.send(Event::Loaded);
                        }
                        Err(e) => {
                            voice_debug!("load failed: {e:#}");
                            let _ = events.send(Event::Failed(describe(&e)));
                        }
                    }
                }
                Command::Transcribe(audio) => {
                    let seconds = audio.len() as f64 / SAMPLE_RATE as f64;
                    voice_debug!("transcribing {seconds:.1}s");
                    let outcome = match &engine {
                        Some(engine) => Ok(engine.transcribe(&audio)),
                        // Unreachable through the state machine, which will not
                        // record before the model is ready — but a channel is a
                        // channel and this is cheaper than a panic.
                        None => Err(anyhow!("the model is not loaded yet")),
                    };
                    let _ = match outcome {
                        Ok(text) => {
                            voice_debug!("heard: {text:?}");
                            events.send(Event::Transcribed(text))
                        }
                        Err(e) => events.send(Event::Failed(describe(&e))),
                    };
                }
                Command::Shutdown => break,
            }
        }
        voice_debug!("voice thread finished");
    }
}

impl Drop for Transcriber {
    fn drop(&mut self) {
        let _ = self.tx.send(Command::Shutdown);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Streaming speech recognition through sherpa-onnx.
///
/// The model is a streaming transducer, so the same recognizer serves both a
/// whole recording handed over at the end (what [`Engine::transcribe`] does)
/// and audio fed while somebody is still talking.
pub struct Engine {
    recognizer: OnlineRecognizer,
}

impl Engine {
    /// Download the model if it is not cached yet, then build the recognizer.
    pub fn load(model: &ModelConfig) -> Result<Self> {
        crate::fetch::ensure(model)?;
        let dir = model.dir();
        let path = |file: &str| Some(dir.join(file).to_string_lossy().into_owned());

        let mut config = OnlineRecognizerConfig::default();
        config.model_config.transducer.encoder = path("encoder.int8.onnx");
        config.model_config.transducer.decoder = path("decoder.int8.onnx");
        config.model_config.transducer.joiner = path("joiner.int8.onnx");
        config.model_config.tokens = path("tokens.txt");
        // Half the cores, at most four: the editor, the language server and a
        // browser are all running too, and past four threads a model this
        // size stops getting faster.
        config.model_config.num_threads = std::thread::available_parallelism()
            .map(|n| (n.get() / 2).clamp(1, 4) as i32)
            .unwrap_or(2);
        config.decoding_method = Some("greedy_search".into());
        config.enable_endpoint = true;

        let recognizer = OnlineRecognizer::create(&config).ok_or_else(|| {
            anyhow!(
                "the speech model in {} could not be loaded (set SMITHY_VOICE_DEBUG=1 for why)",
                dir.display()
            )
        })?;
        Ok(Self { recognizer })
    }

    /// Transcribe a whole recording: 16 kHz mono f32.
    pub fn transcribe(&self, audio: &[f32]) -> String {
        let start = std::time::Instant::now();
        let stream = self.recognizer.create_stream();
        // Fed in the chunks a live microphone would deliver, and finished with
        // a little silence: a streaming model holds back the last few frames
        // until it has seen what follows them.
        for chunk in audio.chunks(SAMPLE_RATE as usize / 5) {
            stream.accept_waveform(SAMPLE_RATE, chunk);
            while self.recognizer.is_ready(&stream) {
                self.recognizer.decode(&stream);
            }
        }
        stream.accept_waveform(SAMPLE_RATE, &[0.0; SAMPLE_RATE as usize * 3 / 10]);
        stream.input_finished();
        while self.recognizer.is_ready(&stream) {
            self.recognizer.decode(&stream);
        }
        let text = self
            .recognizer
            .get_result(&stream)
            .map(|r| r.text)
            .unwrap_or_default();
        voice_debug!(
            "transcribed {:.1}s of audio in {:.2}s",
            audio.len() as f32 / SAMPLE_RATE as f32,
            start.elapsed().as_secs_f32()
        );
        crate::tidy(&text)
    }
}

/// A failure, as a sentence somebody can act on.
///
/// The chain matters here more than usual: "load failed" is useless, whereas
/// "no such host: github.com" says the network is down and "Permission
/// denied" says the cache directory is not writable. Those have entirely
/// different fixes and the outer message distinguishes none of them.
fn describe(error: &anyhow::Error) -> String {
    let mut parts = vec![error.to_string()];
    parts.extend(error.chain().skip(1).map(|cause| cause.to_string()));
    parts.join(": ")
}
