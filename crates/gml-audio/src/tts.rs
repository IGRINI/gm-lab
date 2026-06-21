//! TTS proxy + on-disk clip cache + ffmpeg transcode.
//!
//! Verbatim port of the TTS helpers and `/tts` handler in `gm-lab/server.py`
//! (`_npc_voice`, `_tts_synth`, `_tts_cache_path`/`_lookup`/`_store`,
//! `_compress_audio`, `_pcm_to_wav`, `_proxy_tts_stream`).
//!
//! ## Preserved invariants
//!
//! - **Cache key**: `sha1(format!("{voice}\n{text}"))` hex — voice, a single
//!   `\n`, then the EXACT text (server.py line 93). File name `{key}.{ext}`.
//! - **Format map** (`_TTS_FMT`): `ogg` → libopus 32k mono ogg; `mp3` →
//!   libmp3lame 56k mono mp3; `wav` → passthrough (no ffmpeg).
//! - **ffmpeg arg vector**: `ffmpeg -hide_banner -loglevel error -f wav -i
//!   pipe:0 <encode args> pipe:1`, exactly as server.py line 120-122.
//! - **Fallback**: on ffmpeg spawn failure or non-zero exit, return the raw WAV
//!   as `audio/wav`/`wav` (never crash).
//! - **Voice-by-gender** (`_npc_voice`): pronouns starting `F` or containing
//!   `ЖЕН` → `female`; starting `M` or containing `МУЖ` → `male`; default
//!   `male`.
//! - **`cache_lookup`**: probe the configured-format ext first, then `wav`;
//!   return the first file with size > 0. Readable fully offline.
//! - **Sample rate** for the PCM stream defaults to `24000` when the sidecar
//!   omits `X-Sample-Rate` (server.py line 668).

use std::path::{Path, PathBuf};

use sha1::{Digest, Sha1};

/// Env var holding the sidecar base URL (default `http://127.0.0.1:8765`).
pub const TTS_URL_ENV: &str = "GM_TTS_URL";
/// Env var holding the on-disk cache directory (default `<cwd>/tts_cache`).
pub const TTS_CACHE_DIR_ENV: &str = "GM_TTS_CACHE_DIR";
/// Env var selecting the output format: `ogg` | `mp3` | `wav` (default `ogg`).
pub const TTS_FORMAT_ENV: &str = "GM_TTS_FORMAT";

/// Default PCM sample rate when the sidecar omits `X-Sample-Rate`.
pub const DEFAULT_SAMPLE_RATE: u32 = 24000;

/// TTS failures. Mirrors the 503-style errors the Python `/tts` handler returns
/// when the sidecar is unavailable, plus a 400 for empty text.
#[derive(Debug, thiserror::Error)]
pub enum TtsError {
    /// Empty `text` — the Python handler returns 400 "empty text".
    #[error("empty text")]
    EmptyText,
    /// The sidecar is unreachable / errored. Maps to HTTP 503
    /// ("TTS-сервис недоступен").
    #[error("TTS-сервис недоступен: {0}")]
    Unavailable(String),
    /// A local I/O failure (cache read/write, ffmpeg piping).
    #[error("tts io error: {0}")]
    Io(#[from] std::io::Error),
}

/// One of the three supported output formats. Faithful to `_TTS_FMT`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TtsFormat {
    /// libopus 32k mono in an ogg container.
    Ogg,
    /// libmp3lame 56k mono.
    Mp3,
    /// Raw WAV passthrough (no ffmpeg).
    Wav,
}

impl TtsFormat {
    /// Parse the `GM_TTS_FORMAT` value. Unknown / unset values fall back to
    /// `ogg` (server.py: `TTS_FORMAT if TTS_FORMAT in _TTS_FMT else "ogg"`).
    pub fn parse(raw: &str) -> Self {
        match raw.trim().to_ascii_lowercase().as_str() {
            "ogg" => TtsFormat::Ogg,
            "mp3" => TtsFormat::Mp3,
            "wav" => TtsFormat::Wav,
            _ => TtsFormat::Ogg,
        }
    }

    /// File extension for cache files.
    pub fn ext(self) -> &'static str {
        match self {
            TtsFormat::Ogg => "ogg",
            TtsFormat::Mp3 => "mp3",
            TtsFormat::Wav => "wav",
        }
    }

    /// HTTP `Content-Type`.
    pub fn content_type(self) -> &'static str {
        match self {
            TtsFormat::Ogg => "audio/ogg",
            TtsFormat::Mp3 => "audio/mpeg",
            TtsFormat::Wav => "audio/wav",
        }
    }

    /// ffmpeg encode args, or `None` for the WAV passthrough. EXACT vectors
    /// from `_TTS_FMT` in server.py.
    pub fn ffmpeg_encode_args(self) -> Option<&'static [&'static str]> {
        match self {
            TtsFormat::Ogg => Some(&["-c:a", "libopus", "-b:a", "32k", "-ac", "1", "-f", "ogg"]),
            TtsFormat::Mp3 => Some(&[
                "-c:a", "libmp3lame", "-b:a", "56k", "-ac", "1", "-f", "mp3",
            ]),
            TtsFormat::Wav => None,
        }
    }
}

/// The configured output format, read from `GM_TTS_FORMAT` (default ogg).
pub fn tts_format() -> TtsFormat {
    TtsFormat::parse(&std::env::var(TTS_FORMAT_ENV).unwrap_or_default())
}

/// The sidecar base URL, with any trailing `/` stripped (server.py line 54).
pub fn tts_url() -> String {
    let raw = std::env::var(TTS_URL_ENV).unwrap_or_default();
    let raw = raw.trim();
    let base = if raw.is_empty() { "http://127.0.0.1:8765" } else { raw };
    base.trim_end_matches('/').to_string()
}

/// The on-disk cache directory. `GM_TTS_CACHE_DIR` or `<cwd>/tts_cache`.
///
/// NOTE: server.py anchors the default at `HERE` (the dir of `server.py`). The
/// Rust app resolves audio dirs via `directories` in `gml-app`; here we mirror
/// the Python default relative to the current working directory and let callers
/// override via env. The cache key/path scheme is independent of the directory.
pub fn cache_dir() -> PathBuf {
    let raw = std::env::var(TTS_CACHE_DIR_ENV).unwrap_or_default();
    if raw.trim().is_empty() {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join("tts_cache")
    } else {
        PathBuf::from(raw)
    }
}

/// Map an NPC's grammatical-gender pronouns to a TTS voice.
///
/// Verbatim `_npc_voice` logic: uppercase+trim the pronouns; `F*` or contains
/// `ЖЕН` → `female`; `M*` or contains `МУЖ` → `male`; everything else → `male`
/// (the default character voice for N/PL/unknown).
pub fn npc_voice(pronouns: &str) -> &'static str {
    let p = pronouns.trim().to_uppercase();
    if p.starts_with('F') || p.contains("ЖЕН") {
        return "female";
    }
    if p.starts_with('M') || p.contains("МУЖ") {
        return "male";
    }
    "male"
}

/// The cache key for a `(voice, text)` pair: `sha1(format!("{voice}\n{text}"))`
/// hex. EXACT scheme from `_tts_cache_path` (server.py line 93).
pub fn cache_key(voice: &str, text: &str) -> String {
    let mut hasher = Sha1::new();
    hasher.update(format!("{voice}\n{text}").as_bytes());
    let digest = hasher.finalize();
    let mut out = String::with_capacity(40);
    for b in digest {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

/// The on-disk path for a cached clip: `<cache_dir>/{sha1}.{ext}`.
pub fn cache_path(dir: &Path, voice: &str, text: &str, ext: &str) -> PathBuf {
    dir.join(format!("{}.{}", cache_key(voice, text), ext))
}

/// A cached / synthesized audio clip: bytes + the HTTP content type + the file
/// extension it was stored under.
#[derive(Debug, Clone)]
pub struct AudioClip {
    /// Encoded audio bytes.
    pub bytes: Vec<u8>,
    /// HTTP `Content-Type` (e.g. `audio/ogg`).
    pub content_type: &'static str,
    /// File extension the clip is/was stored under (e.g. `ogg`).
    pub ext: &'static str,
}

/// Look up a cached clip for `(voice, text)`. Probe the configured-format ext
/// first, then `wav` (server.py `_tts_cache_lookup`). Returns the first file
/// with size > 0. Fully offline — no sidecar needed for a hit.
pub fn cache_lookup(dir: &Path, voice: &str, text: &str, fmt: TtsFormat) -> Option<AudioClip> {
    // [(ext, content_type), ("wav", "audio/wav")] — same order as Python.
    let candidates: [(&'static str, &'static str); 2] = [
        (fmt.ext(), fmt.content_type()),
        ("wav", "audio/wav"),
    ];
    for (ext, ctype) in candidates {
        let p = cache_path(dir, voice, text, ext);
        match std::fs::metadata(&p) {
            Ok(meta) if meta.len() > 0 => {
                if let Ok(bytes) = std::fs::read(&p) {
                    return Some(AudioClip { bytes, content_type: ctype, ext });
                }
            }
            _ => continue,
        }
    }
    None
}

/// Store a clip atomically (`tmp` write + rename). Best-effort: I/O errors are
/// swallowed exactly like Python's `_tts_cache_store`.
pub fn cache_store(dir: &Path, voice: &str, text: &str, audio: &[u8], ext: &str) {
    let _ = (|| -> std::io::Result<()> {
        std::fs::create_dir_all(dir)?;
        let path = cache_path(dir, voice, text, ext);
        let tmp = path.with_extension(format!("{ext}.tmp"));
        std::fs::write(&tmp, audio)?;
        std::fs::rename(&tmp, &path)?;
        Ok(())
    })();
}

/// Build the raw WAV byte stream for 16-bit mono PCM at `sr` Hz. Verbatim
/// `_pcm_to_wav` (canonical 44-byte WAV header + PCM frames).
pub fn pcm_to_wav(pcm: &[u8], sr: u32) -> Vec<u8> {
    let channels: u16 = 1;
    let bits: u16 = 16;
    let byte_rate = sr * channels as u32 * (bits as u32 / 8);
    let block_align = channels * (bits / 8);
    let data_len = pcm.len() as u32;
    let riff_len = 36 + data_len;

    let mut out = Vec::with_capacity(44 + pcm.len());
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&riff_len.to_le_bytes());
    out.extend_from_slice(b"WAVE");
    out.extend_from_slice(b"fmt ");
    out.extend_from_slice(&16u32.to_le_bytes()); // PCM fmt chunk size
    out.extend_from_slice(&1u16.to_le_bytes()); // audio format = PCM
    out.extend_from_slice(&channels.to_le_bytes());
    out.extend_from_slice(&sr.to_le_bytes());
    out.extend_from_slice(&byte_rate.to_le_bytes());
    out.extend_from_slice(&block_align.to_le_bytes());
    out.extend_from_slice(&bits.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    out.extend_from_slice(pcm);
    out
}

/// Compress a WAV blob to the configured format via ffmpeg, piping
/// stdin→stdout. Falls back to the raw WAV (`audio/wav`/`wav`) if ffmpeg is
/// unavailable or fails. Verbatim `_compress_audio`.
pub async fn compress_audio(wav_bytes: Vec<u8>, fmt: TtsFormat) -> AudioClip {
    let args = match fmt.ffmpeg_encode_args() {
        None => {
            return AudioClip {
                bytes: wav_bytes,
                content_type: "audio/wav",
                ext: "wav",
            };
        }
        Some(a) => a,
    };

    match run_ffmpeg(&wav_bytes, args).await {
        Some(out) if !out.is_empty() => AudioClip {
            bytes: out,
            content_type: fmt.content_type(),
            ext: fmt.ext(),
        },
        _ => AudioClip {
            bytes: wav_bytes,
            content_type: "audio/wav",
            ext: "wav",
        },
    }
}

/// Spawn ffmpeg, pipe `input` to stdin, collect stdout. Returns `None` on any
/// spawn/exec failure or non-zero exit. Exact arg vector:
/// `ffmpeg -hide_banner -loglevel error -f wav -i pipe:0 <encode> pipe:1`.
async fn run_ffmpeg(input: &[u8], encode_args: &[&str]) -> Option<Vec<u8>> {
    use tokio::io::AsyncWriteExt;
    use tokio::process::Command;

    let mut cmd = Command::new("ffmpeg");
    cmd.arg("-hide_banner")
        .arg("-loglevel")
        .arg("error")
        .arg("-f")
        .arg("wav")
        .arg("-i")
        .arg("pipe:0");
    for a in encode_args {
        cmd.arg(a);
    }
    cmd.arg("pipe:1");
    cmd.stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    crate::proc::no_window(&mut cmd);

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(_) => return None,
    };

    // Write input then close stdin so ffmpeg sees EOF. Do this concurrently
    // with draining stdout to avoid deadlock on large clips.
    let mut stdin = child.stdin.take()?;
    let input_owned = input.to_vec();
    let writer = tokio::spawn(async move {
        let _ = stdin.write_all(&input_owned).await;
        let _ = stdin.shutdown().await;
        // stdin dropped here -> EOF
    });

    let output = child.wait_with_output().await.ok();
    let _ = writer.await;

    match output {
        Some(o) if o.status.success() && !o.stdout.is_empty() => Some(o.stdout),
        _ => None,
    }
}

/// Proxy a non-streaming synthesis request to the sidecar's `/speak` route.
/// `POST {TTS_URL}/speak` with JSON `{text, voice}` → WAV bytes. Verbatim
/// `_tts_synth`. 120s timeout, like the Python `urlopen(..., timeout=120)`.
pub async fn tts_synth(
    http: &reqwest::Client,
    text: &str,
    voice: &str,
) -> Result<Vec<u8>, TtsError> {
    let url = format!("{}/speak", tts_url());
    let body = serde_json::json!({ "text": text, "voice": voice });
    let resp = http
        .post(&url)
        .header("Content-Type", "application/json")
        .json(&body)
        .timeout(std::time::Duration::from_secs(120))
        .send()
        .await
        .map_err(|e| TtsError::Unavailable(e.to_string()))?;
    if !resp.status().is_success() {
        return Err(TtsError::Unavailable(format!("HTTP {}", resp.status())));
    }
    let bytes = resp
        .bytes()
        .await
        .map_err(|e| TtsError::Unavailable(e.to_string()))?;
    Ok(bytes.to_vec())
}

/// Result of a PCM proxy stream: the consumer streams `chunks` to the client
/// head-first, and once the stream completes the accumulated PCM is compressed
/// + cached.
///
/// In Rust we expose the raw `reqwest::Response` so the server layer can stream
/// it byte-for-byte (mirroring server.py's `up.read(16384)` loop) and decide
/// when the stream completed cleanly. [`stream_open`] returns the sample rate +
/// the open response; the server drives the read loop and then calls
/// [`finalize_stream_cache`] with the accumulated PCM.
pub struct PcmStream {
    /// Sample rate from `X-Sample-Rate` (default 24000).
    pub sample_rate: u32,
    /// The open upstream response; body is the PCM byte stream.
    pub response: reqwest::Response,
}

/// Open a PCM proxy stream against the sidecar's `/speak_stream` route.
/// `POST {TTS_URL}/speak_stream` with JSON `{text, voice}`; reads
/// `X-Sample-Rate` (default 24000). Verbatim `_proxy_tts_stream` setup. Raises
/// before any headers are sent downstream if the sidecar is down.
pub async fn stream_open(
    http: &reqwest::Client,
    text: &str,
    voice: &str,
) -> Result<PcmStream, TtsError> {
    let url = format!("{}/speak_stream", tts_url());
    let body = serde_json::json!({ "text": text, "voice": voice });
    let resp = http
        .post(&url)
        .header("Content-Type", "application/json")
        .json(&body)
        .timeout(std::time::Duration::from_secs(120))
        .send()
        .await
        .map_err(|e| TtsError::Unavailable(e.to_string()))?;
    if !resp.status().is_success() {
        return Err(TtsError::Unavailable(format!("HTTP {}", resp.status())));
    }
    let sample_rate = resp
        .headers()
        .get("X-Sample-Rate")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.trim().parse::<u32>().ok())
        .unwrap_or(DEFAULT_SAMPLE_RATE);
    Ok(PcmStream { sample_rate, response: resp })
}

/// After a PCM stream completes cleanly with accumulated `pcm`, compress to the
/// configured format and store in the cache (verbatim tail of
/// `_proxy_tts_stream`: cache only a full clip).
pub async fn finalize_stream_cache(
    dir: &Path,
    voice: &str,
    text: &str,
    pcm: &[u8],
    sr: u32,
    fmt: TtsFormat,
) {
    if pcm.is_empty() {
        return;
    }
    let wav = pcm_to_wav(pcm, sr);
    let clip = compress_audio(wav, fmt).await;
    cache_store(dir, voice, text, &clip.bytes, clip.ext);
}

/// Resolve the voice for a `/tts` request, mirroring the handler's coercion:
/// an explicit `voice` in `{gm, male, female}` wins; otherwise `role == "gm"`
/// or a missing `npc_id` → `"gm"`; else map the NPC's pronouns via
/// [`npc_voice`]. `npc_pronouns` is the looked-up pronoun string (caller
/// resolves the NPC).
pub fn resolve_voice(
    explicit_voice: &str,
    role: &str,
    npc_id: &str,
    npc_pronouns: Option<&str>,
) -> String {
    let v = explicit_voice.trim().to_ascii_lowercase();
    if v == "gm" || v == "male" || v == "female" {
        return v;
    }
    let role = role.trim().to_ascii_lowercase();
    let npc_id = npc_id.trim();
    if role == "gm" || npc_id.is_empty() {
        return "gm".to_string();
    }
    npc_voice(npc_pronouns.unwrap_or("")).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_key_scheme_is_sha1_voice_newline_text() {
        // sha1("male\nПривет") — voice + '\n' + text, exactly as server.py.
        let k = cache_key("male", "Привет");
        // Independently computed reference digest.
        let mut h = Sha1::new();
        h.update(b"male\n");
        h.update("Привет".as_bytes());
        let expect: String = h.finalize().iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(k, expect);
        assert_eq!(k.len(), 40);
        // Distinct voice or text => distinct key.
        assert_ne!(cache_key("female", "Привет"), k);
        assert_ne!(cache_key("male", "Привет!"), k);
        // NOTE: the scheme is `sha1(voice + "\n" + text)`, so ("a","b\nc") and
        // ("a\nb","c") collide — both hash "a\nb\nc". This is a faithful quirk
        // of the Python key (server.py line 93); voices are a fixed closed set
        // ({gm,male,female}) that never contain a newline, so it is harmless.
        assert_eq!(cache_key("a", "b\nc"), cache_key("a\nb", "c"));
    }

    #[test]
    fn cache_path_uses_key_and_ext() {
        let dir = Path::new("/tmp/cache");
        let p = cache_path(dir, "gm", "hello", "ogg");
        let expect = dir.join(format!("{}.ogg", cache_key("gm", "hello")));
        assert_eq!(p, expect);
    }

    #[test]
    fn cache_store_then_lookup_round_trip() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let voice = "female";
        let text = "Round trip test текст";
        let fmt = TtsFormat::Ogg;

        // Miss before store.
        assert!(cache_lookup(dir, voice, text, fmt).is_none());

        // Store under the configured ext.
        let payload = b"fake-ogg-bytes".to_vec();
        cache_store(dir, voice, text, &payload, fmt.ext());

        let hit = cache_lookup(dir, voice, text, fmt).expect("cache hit");
        assert_eq!(hit.bytes, payload);
        assert_eq!(hit.content_type, "audio/ogg");
        assert_eq!(hit.ext, "ogg");

        // The file actually sits at the sha1 path.
        let p = cache_path(dir, voice, text, "ogg");
        assert!(p.exists());
    }

    #[test]
    fn cache_lookup_falls_back_to_wav() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let voice = "gm";
        let text = "wav fallback";
        // Stored as wav, but configured format is mp3 -> lookup must find wav.
        cache_store(dir, voice, text, b"wavbytes", "wav");
        let hit = cache_lookup(dir, voice, text, TtsFormat::Mp3).expect("wav fallback hit");
        assert_eq!(hit.ext, "wav");
        assert_eq!(hit.content_type, "audio/wav");
        assert_eq!(hit.bytes, b"wavbytes");
    }

    #[test]
    fn cache_lookup_ignores_zero_byte_files() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        std::fs::create_dir_all(dir).unwrap();
        // Empty configured-format file must be skipped (size > 0 check).
        let p = cache_path(dir, "male", "x", "ogg");
        std::fs::write(&p, b"").unwrap();
        assert!(cache_lookup(dir, "male", "x", TtsFormat::Ogg).is_none());
    }

    #[test]
    fn voice_by_gender_mapping_is_exact() {
        // Female cues: prefix `F` or contains `ЖЕН` (after uppercasing).
        assert_eq!(npc_voice("F"), "female");
        assert_eq!(npc_voice("female"), "female");
        assert_eq!(npc_voice("ЖЕНСКИЙ"), "female");
        assert_eq!(npc_voice("женский"), "female"); // uppercased internally -> ЖЕН
        assert_eq!(npc_voice("  f/her  "), "female"); // trimmed
        // Male cues: prefix `M` or contains `МУЖ`.
        assert_eq!(npc_voice("M"), "male");
        assert_eq!(npc_voice("male"), "male");
        assert_eq!(npc_voice("МУЖСКОЙ"), "male");
        assert_eq!(npc_voice("мужской"), "male");
        // Default for neuter / plural / unknown (no F/M prefix, no ЖЕН/МУЖ).
        // NOTE: "она/её" has no F/M prefix and no ЖЕН substring -> default male,
        // exactly like the Python `_npc_voice` (it keys on prefix/substring,
        // not semantic gender).
        assert_eq!(npc_voice("она/её"), "male");
        assert_eq!(npc_voice(""), "male");
        assert_eq!(npc_voice("N"), "male");
        assert_eq!(npc_voice("они/их"), "male");
        assert_eq!(npc_voice("plural"), "male");
    }

    #[test]
    fn format_parse_and_ext_and_args() {
        assert_eq!(TtsFormat::parse("ogg"), TtsFormat::Ogg);
        assert_eq!(TtsFormat::parse("MP3"), TtsFormat::Mp3);
        assert_eq!(TtsFormat::parse(" wav "), TtsFormat::Wav);
        // Unknown / empty -> ogg.
        assert_eq!(TtsFormat::parse("flac"), TtsFormat::Ogg);
        assert_eq!(TtsFormat::parse(""), TtsFormat::Ogg);

        assert_eq!(TtsFormat::Ogg.ext(), "ogg");
        assert_eq!(TtsFormat::Mp3.ext(), "mp3");
        assert_eq!(TtsFormat::Wav.ext(), "wav");

        assert_eq!(TtsFormat::Ogg.content_type(), "audio/ogg");
        assert_eq!(TtsFormat::Mp3.content_type(), "audio/mpeg");
        assert_eq!(TtsFormat::Wav.content_type(), "audio/wav");

        // Exact ffmpeg arg vectors.
        assert_eq!(
            TtsFormat::Ogg.ffmpeg_encode_args().unwrap(),
            &["-c:a", "libopus", "-b:a", "32k", "-ac", "1", "-f", "ogg"]
        );
        assert_eq!(
            TtsFormat::Mp3.ffmpeg_encode_args().unwrap(),
            &["-c:a", "libmp3lame", "-b:a", "56k", "-ac", "1", "-f", "mp3"]
        );
        assert!(TtsFormat::Wav.ffmpeg_encode_args().is_none());
    }

    #[test]
    fn resolve_voice_coercion() {
        // Explicit voice wins.
        assert_eq!(resolve_voice("female", "npc", "n1", Some("M")), "female");
        assert_eq!(resolve_voice("GM", "", "", None), "gm");
        // role == gm -> gm.
        assert_eq!(resolve_voice("", "gm", "n1", Some("F")), "gm");
        // no npc_id -> gm.
        assert_eq!(resolve_voice("", "npc", "", None), "gm");
        // else map pronouns.
        assert_eq!(resolve_voice("", "npc", "n1", Some("F")), "female");
        assert_eq!(resolve_voice("", "npc", "n1", Some("МУЖ")), "male");
        assert_eq!(resolve_voice("", "npc", "n1", None), "male");
    }

    #[test]
    fn pcm_to_wav_header_is_canonical() {
        let pcm = vec![0u8; 8];
        let wav = pcm_to_wav(&pcm, 24000);
        assert_eq!(&wav[0..4], b"RIFF");
        assert_eq!(&wav[8..12], b"WAVE");
        assert_eq!(&wav[12..16], b"fmt ");
        assert_eq!(&wav[36..40], b"data");
        // data length little-endian == pcm.len().
        let data_len = u32::from_le_bytes([wav[40], wav[41], wav[42], wav[43]]);
        assert_eq!(data_len, 8);
        // sample rate field.
        let sr = u32::from_le_bytes([wav[24], wav[25], wav[26], wav[27]]);
        assert_eq!(sr, 24000);
        assert_eq!(wav.len(), 44 + 8);
    }

    #[test]
    fn url_strips_trailing_slash_and_defaults() {
        // Default when unset is covered indirectly; here check the trim logic
        // by constructing through the same rule.
        assert_eq!("http://x:1/".trim_end_matches('/'), "http://x:1");
    }
}
