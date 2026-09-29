use anyhow::{anyhow, Result};
use sherpa_onnx::{OnlineRecognizer, OnlineRecognizerConfig, OnlineStream};
use std::sync::mpsc;

use crate::model::ModelConfig;
use crate::voice_debug;

/// The rate the recognizer works at. Audio at any other rate is resampled by
/// sherpa-onnx on the way in.
pub const SAMPLE_RATE: i32 = 16_000;

/// How long a pause, after something has been said, ends dictation.
pub const PAUSE_ENDS_DICTATION_SECONDS: f32 = 1.2;
/// How long the microphone stays open if nothing is said at all.
pub const SILENCE_CLOSES_MICROPHONE_SECONDS: f32 = 5.0;
/// The longest single dictation before it is finished regardless.
pub const LONGEST_DICTATION_SECONDS: f32 = 60.0;

/// What the supervisor is told to do.
pub enum Command {
    Load(ModelConfig),
    /// A whole recording, 16 kHz mono f32.
    Transcribe(Vec<f32>),
    /// Start a live dictation: audio follows as [`Command::Audio`].
    Begin,
    /// Mono f32 samples at `rate`, as the microphone delivered them.
    Audio {
        samples: Vec<f32>,
        rate: i32,
    },
    /// The microphone was closed by hand: finish what was said.
    Finish,
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
    /// What has been heard so far in a live dictation; replaces the last one.
    Partial(String),
    /// The final text. In a live dictation this also means "close the
    /// microphone": it is sent when a pause ends the dictation, or when
    /// [`Command::Finish`] asks for it. Empty when nothing was said.
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

    /// Transcribe a whole recording. Answers with [`Event::Transcribed`].
    pub fn transcribe(&self, audio_f32_16khz: Vec<f32>) {
        let _ = self.tx.send(Command::Transcribe(audio_f32_16khz));
    }

    /// Start a live dictation. Returns what the microphone should call with
    /// each block of mono samples; answers with [`Event::Partial`] as words
    /// arrive and [`Event::Transcribed`] when the dictation ends.
    pub fn begin(&self, rate: u32) -> impl FnMut(&[f32]) + Send + 'static {
        let _ = self.tx.send(Command::Begin);
        let tx = self.tx.clone();
        let rate = rate as i32;
        move |samples: &[f32]| {
            let _ = tx.send(Command::Audio {
                samples: samples.to_vec(),
                rate,
            });
        }
    }

    /// The microphone was closed by hand: finish the live dictation.
    pub fn finish(&self) {
        let _ = self.tx.send(Command::Finish);
    }

    fn run_loop(rx: mpsc::Receiver<Command>, events: crossbeam_channel::Sender<Event>) {
        let mut engine: Option<Engine> = None;
        let mut live: Option<Live> = None;

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
                    let _ = match &engine {
                        Some(engine) => events.send(Event::Transcribed(engine.transcribe(&audio))),
                        // Unreachable through the state machine, which will not
                        // record before the model is ready — but a channel is a
                        // channel and this is cheaper than a panic.
                        None => events.send(Event::Failed("the model is not loaded yet".into())),
                    };
                }
                Command::Begin => match &engine {
                    Some(engine) => {
                        voice_debug!("listening");
                        live = Some(Live {
                            stream: engine.recognizer.create_stream(),
                            heard: String::new(),
                        });
                    }
                    None => {
                        let _ = events.send(Event::Failed("the model is not loaded yet".into()));
                    }
                },
                Command::Audio { samples, rate } => {
                    // Audio after the dictation ended — the few milliseconds
                    // before the microphone is closed — is simply dropped.
                    let (Some(engine), Some(current)) = (&engine, &mut live) else {
                        continue;
                    };
                    current.stream.accept_waveform(rate, &samples);
                    engine.decode(&current.stream);
                    let heard = engine.text(&current.stream);
                    if heard != current.heard {
                        current.heard = heard.clone();
                        let _ = events.send(Event::Partial(heard.clone()));
                    }
                    if engine.recognizer.is_endpoint(&current.stream) {
                        voice_debug!("a pause ended the dictation: {heard:?}");
                        live = None;
                        let _ = events.send(Event::Transcribed(heard));
                    }
                }
                Command::Finish => {
                    // Already ended by a pause: its text was sent then.
                    let (Some(engine), Some(current)) = (&engine, live.take()) else {
                        continue;
                    };
                    let text = engine.finish(&current.stream);
                    voice_debug!("closed by hand: {text:?}");
                    let _ = events.send(Event::Transcribed(text));
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

/// A dictation in progress.
struct Live {
    stream: OnlineStream,
    /// The last partial text sent, so an unchanged one is not sent again.
    heard: String,
}

/// Streaming speech recognition through sherpa-onnx.
///
/// The model is a streaming transducer, so the same recognizer serves both a
/// whole recording handed over at the end ([`Engine::transcribe`]) and audio
/// fed while somebody is still talking.
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
        // sherpa-onnx's endpoint rules: nothing said at all (rule 1), a pause
        // after speech (rule 2), and a ceiling on one utterance (rule 3).
        config.enable_endpoint = true;
        config.rule1_min_trailing_silence = SILENCE_CLOSES_MICROPHONE_SECONDS;
        config.rule2_min_trailing_silence = PAUSE_ENDS_DICTATION_SECONDS;
        config.rule3_min_utterance_length = LONGEST_DICTATION_SECONDS;

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
        // Fed in the chunks a live microphone would deliver.
        for chunk in audio.chunks(SAMPLE_RATE as usize / 5) {
            stream.accept_waveform(SAMPLE_RATE, chunk);
            self.decode(&stream);
        }
        let text = self.finish(&stream);
        voice_debug!(
            "transcribed {:.1}s of audio in {:.2}s",
            audio.len() as f32 / SAMPLE_RATE as f32,
            start.elapsed().as_secs_f32()
        );
        text
    }

    fn decode(&self, stream: &OnlineStream) {
        while self.recognizer.is_ready(stream) {
            self.recognizer.decode(stream);
        }
    }

    fn text(&self, stream: &OnlineStream) -> String {
        let raw = self
            .recognizer
            .get_result(stream)
            .map(|r| r.text)
            .unwrap_or_default();
        crate::tidy(&raw)
    }

    /// End the input and return everything that was said. A streaming model
    /// holds back the last few frames until it has seen what follows them, so
    /// a little silence goes in first.
    fn finish(&self, stream: &OnlineStream) -> String {
        stream.accept_waveform(SAMPLE_RATE, &[0.0; SAMPLE_RATE as usize * 3 / 10]);
        stream.input_finished();
        self.decode(stream);
        self.text(stream)
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
