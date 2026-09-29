//! The microphone, wired to the agent panel.
//!
//! `smithy-voice` turns audio into a string and knows nothing about buttons;
//! this is the other half — one press handler, and the bridge that carries
//! results from the recognizer's thread onto the screen.
//!
//! What a press *means* is [`smithy_voice::press`], a pure function tested
//! without a microphone. What it *does* is here, because that is where the
//! channels and the signals live.
//!
//! Dictation is live: the words appear in the prompt as they are said, and a
//! pause ends it — the microphone closes by itself. Pressing again closes it
//! early.

use std::cell::RefCell;
use std::rc::Rc;

use floem::reactive::{RwSignal, SignalGet, SignalUpdate};
use smithy_voice::audio::{AudioRecorder, LiveHandle};
use smithy_voice::inference::{Event, Transcriber};
use smithy_voice::{press, Press, Voice};

/// Everything the microphone needs to stay alive between presses.
///
/// **No recorder here, deliberately.** It used to hold one, resolved once by
/// `AudioRecorder::new()` at launch and kept for the life of the process, which
/// meant the input device was whatever existed at startup. Connect AirPods
/// afterwards and they were never found — the only cure was relaunching the
/// editor. The device is found at the press that opens it.
pub struct VoiceControl {
    transcriber: Transcriber,
    /// Open only while dictating; dropping it closes the microphone.
    microphone: Rc<RefCell<Option<LiveHandle>>>,
    /// What the prompt held when this dictation began. The live text is shown
    /// after it and replaced as it changes, so nothing typed is lost.
    before: Rc<RefCell<String>>,
    state: RwSignal<Voice>,
    input: RwSignal<String>,
}

impl VoiceControl {
    /// Start the recognizer's thread and bridge its events onto the UI.
    ///
    /// The thread is spawned now and the *model* is not — nothing is fetched
    /// until the first press, so a launch costs a thread and nothing else.
    pub fn new(state: RwSignal<Voice>, input: RwSignal<String>) -> Rc<Self> {
        let (tx, rx) = crossbeam_channel::unbounded::<Event>();
        let (tick, inbox) = crate::app_state::bridge(rx);
        let microphone: Rc<RefCell<Option<LiveHandle>>> = Rc::new(RefCell::new(None));
        let before = Rc::new(RefCell::new(String::new()));

        {
            let microphone = Rc::clone(&microphone);
            let before = Rc::clone(&before);
            floem::reactive::Effect::new(move |_| {
                tick.get();
                for event in crate::app_state::drain(&inbox) {
                    match event {
                        Event::Loaded => state.set(Voice::Ready),
                        // Shown as it is heard, after whatever was already
                        // typed; each partial replaces the last.
                        Event::Partial(text) => {
                            input.set(smithy_voice::append(&before.borrow(), &text));
                        }
                        // The dictation is over, by a pause or by a press:
                        // close the microphone and keep the final text.
                        Event::Transcribed(text) => {
                            microphone.borrow_mut().take();
                            input.set(smithy_voice::append(&before.borrow(), &text));
                            state.set(Voice::Ready);
                        }
                        Event::Failed(why) => {
                            microphone.borrow_mut().take();
                            state.set(Voice::Failed(why));
                        }
                    }
                }
            });
        }

        Rc::new(Self {
            transcriber: Transcriber::new(tx),
            microphone,
            before,
            state,
            input,
        })
    }

    /// One press of the microphone, or of its hotkey.
    pub fn press(&self) {
        match press(&self.state.get_untracked()) {
            Press::LoadModel => {
                self.state.set(Voice::Loading);
                self.transcriber.load(smithy_voice::ModelConfig::default());
            }
            // The device is resolved *here*, on the press that needs it:
            // enumeration costs milliseconds, and paying it per press is what
            // lets a headset connected after launch actually be found.
            //
            // **Not "no microphone".** This error covers every reason the input
            // device could not be opened — none selected, permission never
            // granted, another process holding it exclusively — and naming the
            // one cause it usually is *not* sends you looking at hardware. The
            // panel puts the detail under a hover.
            Press::StartRecording => {
                *self.before.borrow_mut() = self.input.get_untracked();
                let opened = AudioRecorder::new().and_then(|recorder| {
                    let sink = self.transcriber.begin(recorder.sample_rate());
                    recorder.start_streaming(sink)
                });
                match opened {
                    Ok(handle) => {
                        *self.microphone.borrow_mut() = Some(handle);
                        self.state.set(Voice::Listening);
                    }
                    // The recognizer may already have begun a dictation; it is
                    // left to the next `begin` to replace. Finishing it would
                    // send an empty result that turned this failure back into
                    // `Ready` before anyone read it.
                    Err(e) => self
                        .state
                        .set(Voice::Failed(format!("microphone unavailable: {e}"))),
                }
            }
            // Closed by hand before a pause did it: the last words are
            // finished on the recognizer's thread and arrive as `Transcribed`.
            Press::StopAndTranscribe => {
                self.microphone.borrow_mut().take();
                self.state.set(Voice::Transcribing);
                self.transcriber.finish();
            }
            Press::Ignore => {}
        }
    }
}
