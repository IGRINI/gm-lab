//! gml-audio — STT (speech-to-text) + TTS (text-to-speech) proxy/cache/sidecar.
//!
//! Faithful port of the audio subsystem from `gm-lab` (PORT_PLAN.md §3.2 TTS
//! sidecar / ffmpeg / STT rows; risks #2 and #7). Modules:
//!
//! - [`tts`] — voice-by-gender mapping, on-disk clip cache, sidecar proxy
//!   (`/speak`, `/speak_stream`), and ffmpeg transcode. Ported verbatim from
//!   the TTS helpers + `/tts` handler in `gm-lab/server.py`. The disk cache is
//!   readable offline (a cache hit needs no sidecar).
//! - [`sidecar`] — a cross-platform manager that spawns the faster-qwen3-tts
//!   Python sidecar once (guarded by a `OnceCell`), polls a health endpoint
//!   until ready, exposes readiness, and kills the process tree on shutdown.
//!   New design (no Python reference) — see PORT_PLAN §3.2 / risk #7.
//! - [`stt`] — port of `gm-lab/codex_transcribe.py`: multipart POST to the
//!   ChatGPT `/backend-api/transcribe` endpoint behind a Chrome TLS/JA3
//!   impersonation client (`wreq`), authenticated with the Codex subscription
//!   OAuth token. Gated behind the `stt` cargo feature (default on); with the
//!   feature off, a stub returns a typed "STT unavailable" error so the crate
//!   ALWAYS builds.
//!
//! The whole app must run FULLY without TTS: when TTS is disabled or the sidecar
//! cannot start, every TTS path returns a clean 503-style [`tts::TtsError`] and
//! the rest of the app is unaffected.

pub mod proc;
pub mod sidecar;
pub mod stt;
pub mod tts;

pub use sidecar::{Sidecar, SidecarConfig, SidecarError, SidecarState};
pub use stt::{transcribe, TranscribeError};
pub use tts::{
    cache_lookup, cache_path, cache_store, compress_audio, npc_voice, pcm_to_wav, tts_format,
    tts_synth, AudioClip, TtsError, TtsFormat, TTS_CACHE_DIR_ENV, TTS_FORMAT_ENV, TTS_URL_ENV,
};
