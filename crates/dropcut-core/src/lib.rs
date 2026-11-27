//! DropCut core engine.
//!
//! Everything that touches media lives here so the desktop app and the CLI
//! share one implementation:
//!
//! * [`media`]: probing files and decoding audio through ffmpeg
//! * [`silence`]: finding quiet stretches and building waveform peaks
//! * [`timeline`]: the edit decision list (what to keep, what to drop)
//! * [`transcript`]: word-level transcripts and transcription (Whisper)
//! * [`profanity`]: finding and masking swear words
//! * [`captions`]: grouping words into caption lines and writing SRT
//! * [`burn`]: animated captions burned into the video (ASS)
//! * [`publish`]: captions, hashtags and the manifest a scheduler reads
//! * [`reframe`]: finding and following the speaker's face for 9:16 crops
//! * [`zoom`]: automatic punch-in zooms
//! * [`shorts`]: finding the best short clips and framing them 9:16
//! * [`render`]: turning a timeline into a finished video with ffmpeg
//! * [`models`]: the Whisper model catalogue and downloads

pub mod burn;
pub mod captions;
pub mod error;
pub mod media;
pub mod models;
pub mod profanity;
pub mod publish;
pub mod reframe;
pub mod render;
pub mod shorts;
pub mod silence;
pub mod timeline;
pub mod transcript;
pub mod zoom;

pub use error::{Error, Result};
pub use media::{FfmpegPaths, MediaInfo};
pub use timeline::{Region, RegionKind, Span, Timeline};
pub use transcript::{Transcript, Word};
