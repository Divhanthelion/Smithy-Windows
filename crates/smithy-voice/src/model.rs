//! Which model, and where it lives.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// The locally-embedded speech recognition model.
///
/// Downloaded on first use, checked against its published SHA-256, unpacked
/// and cached on disk, so the second launch is instant and every launch after
/// that works offline — the same local-first rule the agent follows.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelConfig {
    /// Where downloaded models are cached; each unpacks into a directory of
    /// its own name beneath this.
    pub cache_dir: String,
    /// The model's name, which is also its directory and its archive's stem.
    pub name: String,
    /// Where the `.tar.bz2` archive is downloaded from.
    pub url: String,
    /// The archive's SHA-256, as published with the release. A download that
    /// does not match is discarded, not unpacked.
    pub sha256: String,
    /// The archive's size in bytes, for the progress sentence and as a first
    /// check before hashing.
    pub size: u64,
}

impl Default for ModelConfig {
    fn default() -> Self {
        Self {
            cache_dir: default_cache_dir(),
            // NVIDIA's Nemotron Speech Streaming EN 0.6B, exported to ONNX and
            // quantized to int8 by the sherpa-onnx project. The 560 ms chunk
            // variant: 7.07% average word error rate on the Open ASR
            // leaderboard's sets, against 7.83% for Whisper large-v3-turbo,
            // which this replaces — at about a third of its memory, and
            // streaming. A shorter chunk shows words sooner and is less
            // accurate; for dictation half a second of lag is the better deal.
            name: "sherpa-onnx-nemotron-speech-streaming-en-0.6b-560ms-int8-2026-04-25".into(),
            url: "https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/\
                  sherpa-onnx-nemotron-speech-streaming-en-0.6b-560ms-int8-2026-04-25.tar.bz2"
                .into(),
            sha256: "78e2b79fcf7271553a74402a76b771b09ea40117a39566a79f52235b23db6358".into(),
            size: 463_945_051,
        }
    }
}

impl ModelConfig {
    /// The unpacked model.
    pub fn dir(&self) -> PathBuf {
        PathBuf::from(&self.cache_dir).join(&self.name)
    }

    /// The files the recognizer is built from, all inside [`Self::dir`].
    pub const FILES: [&'static str; 4] = [
        "encoder.int8.onnx",
        "decoder.int8.onnx",
        "joiner.int8.onnx",
        "tokens.txt",
    ];

    /// Whether every file the recognizer needs is already on disk.
    pub fn is_cached(&self) -> bool {
        let dir = self.dir();
        Self::FILES.iter().all(|f| dir.join(f).is_file())
    }
}

/// Alongside everything else Smithy keeps, rather than in a directory of its
/// own — one place to delete when someone wants their disk back.
fn default_cache_dir() -> String {
    std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .map(|home| format!("{home}/.local/share/smithy/models"))
        .unwrap_or_else(|_| "models".to_string())
}

/// Report what the voice layer is doing, when `SMITHY_VOICE_DEBUG=1`.
///
/// The same pattern as `SMITHY_KEY_DEBUG` and `SMITHY_SQUIGGLE_DEBUG`: model
/// loading takes tens of seconds the first time and involves a network, a
/// cache, a device and an archive, and every one of those failing looks
/// identical from the outside — a mic button that does nothing.
#[macro_export]
macro_rules! voice_debug {
    ($($arg:tt)*) => {
        if std::env::var("SMITHY_VOICE_DEBUG").is_ok_and(|v| v != "0") {
            eprintln!("[voice] {}", format!($($arg)*));
        }
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The cache belongs with the rest of Smithy's data. A model is hundreds of
    /// megabytes and somebody will want to find it.
    #[test]
    fn the_model_is_cached_under_smithys_own_data_directory() {
        let dir = ModelConfig::default().cache_dir;
        assert!(
            dir.contains("smithy"),
            "cached at {dir}, which is nobody's guess"
        );
    }

    /// The URL and the directory name have to agree, or a download unpacks
    /// somewhere the recognizer never looks.
    #[test]
    fn the_archive_unpacks_into_the_directory_the_model_is_read_from() {
        let config = ModelConfig::default();
        assert!(config.url.ends_with(&format!("{}.tar.bz2", config.name)));
        assert_eq!(config.sha256.len(), 64);
    }

    #[test]
    fn a_model_is_cached_only_when_every_file_is_there() {
        let tmp = tempfile::tempdir().unwrap();
        let config = ModelConfig {
            cache_dir: tmp.path().to_string_lossy().into_owned(),
            ..ModelConfig::default()
        };
        assert!(!config.is_cached());
        std::fs::create_dir_all(config.dir()).unwrap();
        for f in &ModelConfig::FILES[..3] {
            std::fs::write(config.dir().join(f), b"x").unwrap();
        }
        assert!(!config.is_cached(), "tokens.txt is still missing");
        std::fs::write(config.dir().join("tokens.txt"), b"x").unwrap();
        assert!(config.is_cached());
    }
}
