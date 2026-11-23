//! Whisper model catalogue and downloads.
//!
//! Models are too big to ship in the installer, so the app downloads the one
//! you pick on first use into its data folder.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ModelInfo {
    pub id: &'static str,
    pub label: &'static str,
    pub size_mb: u32,
    pub note: &'static str,
}

pub const MODELS: &[ModelInfo] = &[
    ModelInfo { id: "base", label: "Base", size_mb: 142, note: "Fast, fine for clear audio. Good on CPU." },
    ModelInfo { id: "small", label: "Small", size_mb: 466, note: "Better accuracy, still quick." },
    ModelInfo {
        id: "large-v3-turbo-q5_0",
        label: "Large v3 Turbo",
        size_mb: 547,
        note: "Best accuracy. Recommended with an NVIDIA GPU or Apple Silicon.",
    },
    ModelInfo { id: "large-v3-turbo", label: "Large v3 Turbo (full)", size_mb: 1550, note: "Slightly better than the compressed version, 3x the size." },
];

pub const DEFAULT_MODEL: &str = "large-v3-turbo-q5_0";

pub fn find(id: &str) -> Option<&'static ModelInfo> {
    MODELS.iter().find(|m| m.id == id)
}

pub fn file_name(id: &str) -> String {
    format!("ggml-{id}.bin")
}

pub fn url(id: &str) -> String {
    format!("https://huggingface.co/ggerganov/whisper.cpp/resolve/main/{}", file_name(id))
}

pub fn path_in(dir: &Path, id: &str) -> PathBuf {
    dir.join(file_name(id))
}

pub fn is_downloaded(dir: &Path, id: &str) -> bool {
    path_in(dir, id).metadata().map(|m| m.len() > 1_000_000).unwrap_or(false)
}

#[cfg(feature = "download")]
pub use download_impl::download;

#[cfg(feature = "download")]
mod download_impl {
    use std::io::{Read, Write};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    use super::*;
    use crate::{Error, Result};

    /// Download model `id` into `dir`. `progress(done_bytes, total_bytes)`.
    /// Writes to a `.part` file first so a cancelled download never looks complete.
    pub fn download(
        dir: &Path,
        id: &str,
        mut progress: impl FnMut(u64, Option<u64>),
        cancel: Option<Arc<AtomicBool>>,
    ) -> Result<PathBuf> {
        if find(id).is_none() {
            return Err(Error::Invalid(format!("unknown model {id}")));
        }
        std::fs::create_dir_all(dir)?;
        let dest = path_in(dir, id);
        let part = dest.with_extension("bin.part");

        let response = ureq::get(&url(id)).call().map_err(|e| Error::Download(e.to_string()))?;
        let total = response
            .headers()
            .get("content-length")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<u64>().ok());
        let mut reader = response.into_body().into_reader();
        let mut file = std::fs::File::create(&part)?;
        let mut buf = vec![0u8; 1 << 20];
        let mut done = 0u64;
        loop {
            if cancel.as_ref().is_some_and(|c| c.load(Ordering::Relaxed)) {
                drop(file);
                let _ = std::fs::remove_file(&part);
                return Err(Error::Cancelled);
            }
            let n = reader.read(&mut buf).map_err(|e| Error::Download(e.to_string()))?;
            if n == 0 {
                break;
            }
            file.write_all(&buf[..n])?;
            done += n as u64;
            progress(done, total);
        }
        file.flush()?;
        drop(file);
        if total.is_some_and(|t| t != done) {
            let _ = std::fs::remove_file(&part);
            return Err(Error::Download("connection dropped before the download finished".into()));
        }
        std::fs::rename(&part, &dest)?;
        Ok(dest)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_model_is_in_catalogue() {
        assert!(find(DEFAULT_MODEL).is_some());
        assert!(url(DEFAULT_MODEL).ends_with("ggml-large-v3-turbo-q5_0.bin"));
    }
}
