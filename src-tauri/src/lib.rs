// Tauri v2 library entry point: Android/iOS builds require a lib target.
// Application wiring lives in `app()`; `run()` is the platform entry point.
#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    app();
}

use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager, State};
use tokio::{    io::{AsyncBufReadExt, BufReader},
    process::Command as TokioCommand,
    sync::Mutex,
};

#[cfg(windows)]
use std::os::windows::process::CommandExt as _;

const YTDLP_EXE_URL: &str =
    "https://github.com/yt-dlp/yt-dlp/releases/latest/download/yt-dlp.exe";

// ---------------------------------------------------------------------------
// yt-dlp binary management
// ---------------------------------------------------------------------------

fn app_bin_dir(app: &AppHandle) -> PathBuf {
    let base = app
        .path()
        .app_data_dir()
        .unwrap_or_else(|_| std::env::temp_dir().join("yt-downloader"));
    base.join("bin")
}

fn candidate_paths(app: &AppHandle) -> Vec<PathBuf> {
    let mut v = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            v.push(dir.join("yt-dlp.exe"));
            v.push(dir.join("yt-dlp"));
        }
    }
    // dev-time convenience: repo binaries folder
    v.push(PathBuf::from("binaries").join("yt-dlp.exe"));
    let b = app_bin_dir(app);
    v.push(b.join("yt-dlp.exe"));
    #[cfg(not(windows))]
    v.push(b.join("yt-dlp"));
    v
}

fn hide_window(cmd: &mut TokioCommand) {
    #[cfg(windows)]
    cmd.creation_flags(0x08000000); // CREATE_NO_WINDOW (inherent on tokio::process::Command)
    #[cfg(not(windows))]
    let _ = cmd;
}

async fn binary_usable(path: &Path) -> bool {
    let mut cmd = TokioCommand::new(path);
    hide_window(&mut cmd);
    match tokio::time::timeout(Duration::from_secs(12), cmd.arg("--version").output()).await
    {
        Ok(Ok(out)) => out.status.success(),
        _ => false,
    }
}

async fn resolve_ytdlp(app: &AppHandle) -> Option<PathBuf> {
    for p in candidate_paths(app) {
        if p.exists() && binary_usable(&p).await {
            return Some(p);
        }
    }
    // PATH fallback (yt-dlp installed separately / pip)
    let probe_name = if cfg!(windows) { "yt-dlp.exe" } else { "yt-dlp" };
    let mut cmd = TokioCommand::new(probe_name);
    hide_window(&mut cmd);
    match tokio::time::timeout(Duration::from_secs(12), cmd.arg("--version").output()).await
    {
        Ok(Ok(out)) if out.status.success() => Some(PathBuf::from(probe_name)),
        _ => None,
    }
}

#[derive(Serialize, Clone)]
struct SetupEvent {
    percent: Option<f64>,
    message: String,
}

#[tauri::command]
async fn ytdlp_status(app: AppHandle) -> Result<serde_json::Value, String> {
    match resolve_ytdlp(&app).await {
        Some(p) => {
            let mut cmd = TokioCommand::new(&p);
            hide_window(&mut cmd);
            let ver = cmd
                .arg("--version")
                .output()
                .await
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
                .unwrap_or_default();
            Ok(serde_json::json!({
                "available": true,
                "path": p.to_string_lossy(),
                "version": ver,
            }))
        }
        None => Ok(serde_json::json!({ "available": false })),
    }
}

#[tauri::command]
async fn ensure_ytdlp(app: AppHandle) -> Result<String, String> {
    if let Some(p) = resolve_ytdlp(&app).await {
        return Ok(p.to_string_lossy().to_string());
    }
    #[cfg(not(windows))]
    return Err(
        "yt-dlp was not found. Please install it (https://github.com/yt-dlp/yt-dlp) and restart the app."
            .to_string(),
    );

    #[cfg(windows)]
    {
        use futures_util::StreamExt;
        let dir = app_bin_dir(&app);
        std::fs::create_dir_all(&dir).map_err(|e| format!("Cannot create folder: {e}"))?;
        let dest = dir.join("yt-dlp.exe");
        let tmp = dir.join("yt-dlp.exe.part");

        let emit = |percent: Option<f64>, message: &str| {
            let _ = app.emit(
                "ytdlp-setup",
                SetupEvent {
                    percent,
                    message: message.to_string(),
                },
            );
        };
        emit(None, "Downloading yt-dlp engine…");

        let resp = reqwest::get(YTDLP_EXE_URL)
            .await
            .map_err(|e| format!("Download failed: {e}"))?;
        if !resp.status().is_success() {
            return Err(format!("Download failed: HTTP {}", resp.status()));
        }
        let total = resp.content_length().unwrap_or(0);
        let mut stream = resp.bytes_stream();
        let mut file = tokio::fs::File::create(&tmp)
            .await
            .map_err(|e| format!("Cannot write file: {e}"))?;
        let mut done: u64 = 0;
        use tokio::io::AsyncWriteExt;
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|e| format!("Download failed: {e}"))?;
            file.write_all(&chunk)
                .await
                .map_err(|e| format!("Cannot write file: {e}"))?;
            done += chunk.len() as u64;
            if total > 0 {
                emit(
                    Some(done as f64 / total as f64 * 100.0),
                    &format!(
                        "Downloading yt-dlp engine… {:.1} / {:.1} MB",
                        done as f64 / 1_048_576.0,
                        total as f64 / 1_048_576.0
                    ),
                );
            }
        }
        file.flush().await.map_err(|e| e.to_string())?;
        drop(file);
        tokio::fs::rename(&tmp, &dest)
            .await
            .map_err(|e| format!("Cannot install yt-dlp: {e}"))?;

        if !binary_usable(&dest).await {
            return Err("Downloaded yt-dlp but it would not run.".to_string());
        }
        emit(Some(100.0), "Engine ready");
        Ok(dest.to_string_lossy().to_string())
    }
}

// ---------------------------------------------------------------------------
// Video info / formats
// ---------------------------------------------------------------------------

#[derive(Serialize, Clone)]
struct FormatInfo {
    id: String,
    ext: String,
    label: String,
    width: Option<u32>,
    height: Option<u32>,
    fps: Option<f64>,
    vcodec: String,
    acodec: String,
    filesize: Option<u64>,
    tbr: Option<f64>,
    abr: Option<f64>,
    vbr: Option<f64>,
    note: String,
    protocol: String,
    has_video: bool,
    has_audio: bool,
    kind: String, // "combined" | "video" | "audio" | "other"
}

#[derive(Serialize, Clone)]
struct VideoInfo {
    id: String,
    title: String,
    uploader: String,
    channel: String,
    site: String,
    extractor: String,
    duration: Option<u64>,
    views: Option<u64>,
    likes: Option<u64>,
    upload_date: String,
    description: String,
    thumbnail: String,
    url: String,
    format_count: usize,
    formats: Vec<FormatInfo>,
}

fn s(v: &serde_json::Value, k: &str) -> String {
    v.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string()
}
/// First non-empty string (proper fallback, unlike lexicographic max).
fn pick(items: Vec<String>) -> String {
    items.into_iter().find(|x| !x.trim().is_empty()).unwrap_or_default()
}
fn opt_u64(v: &serde_json::Value, k: &str) -> Option<u64> {
    v.get(k).and_then(|x| x.as_u64())
}
fn opt_f64(v: &serde_json::Value, k: &str) -> Option<f64> {
    v.get(k).and_then(|x| x.as_f64())
}
fn opt_u32(v: &serde_json::Value, k: &str) -> Option<u32> {
    v.get(k)
        .and_then(|x| x.as_u64())
        .map(|x| x as u32)
}

fn codec_short(c: &str) -> String {
    let c = c.to_lowercase();
    if c == "none" || c.is_empty() {
        return "—".to_string();
    }
    for known in ["av01", "vp9", "vp8", "avc1", "h264", "h265", "hevc", "opus", "mp4a", "aac", "vorbis", "flac", "alac", "ac-3", "ec-3", "mp3"] {
        if c.starts_with(known) {
            return known.to_string();
        }
    }
    c.split('.').next().unwrap_or(&c).to_string()
}

fn build_label(f: &serde_json::Value, has_video: bool, has_audio: bool) -> String {
    if has_video {
        if let Some(h) = opt_u32(f, "height") {
            let tag = if has_audio { "" } else { " (no audio)" };
            let proto = s(f, "format_note");
            let extra = if proto.contains("HDR") { " HDR" } else { "" };
            return format!("{h}p{extra}{tag}");
        }
        let note = s(f, "format_note");
        if !note.is_empty() {
            return note;
        }
        return pick(vec![s(f, "resolution"), s(f, "format_id")]);
    }
    if has_audio {
        if let Some(a) = opt_f64(f, "abr") {
            return format!("{} kbps", a.round() as u64);
        }
        let note = s(f, "format_note");
        if !note.is_empty() {
            return note;
        }
        return "audio".to_string();
    }
    pick(vec![s(f, "format_note"), s(f, "resolution"), s(f, "format_id")])
}

#[derive(Serialize, Clone)]
struct PlaylistEntry {
    id: String,
    title: String,
    url: String,
    duration: Option<u64>,
    thumbnail: String,
    index: usize,
}

#[derive(Serialize, Clone)]
struct PlaylistInfo {
    title: String,
    uploader: String,
    count: usize,
    thumbnail: String,
    entries: Vec<PlaylistEntry>,
}

#[tauri::command]
async fn fetch_playlist(
    app: AppHandle,
    url: String,
    cookies_from: Option<String>,
) -> Result<PlaylistInfo, String> {
    let url = url.trim().to_string();
    if url.is_empty() {
        return Err("Please paste a playlist link first.".to_string());
    }
    let bin = match resolve_ytdlp(&app).await {
        Some(p) => p,
        None => PathBuf::from(ensure_ytdlp(app.clone()).await?),
    };

    // --flat-playlist lists entries without resolving every video, so this
    // stays fast even for 500-video playlists.
    let mut cmd = TokioCommand::new(&bin);
    hide_window(&mut cmd);
    cmd.args(["--flat-playlist", "--no-warnings", "--socket-timeout", "25", "-J"]);
    if let Some(browser) = cookies_from.as_deref().filter(|b| !b.is_empty() && *b != "none") {
        cmd.args(["--cookies-from-browser", browser]);
    }
    cmd.arg(&url)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let out = tokio::time::timeout(Duration::from_secs(120), cmd.output())
        .await
        .map_err(|_| "Timed out while reading the playlist.".to_string())?
        .map_err(|e| format!("Failed to run yt-dlp: {e}"))?;

    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        let msg = err
            .lines()
            .rev()
            .find(|l| l.contains("ERROR"))
            .map(|l| l.split("ERROR:").nth(1).unwrap_or(l).trim().trim_start_matches('[').to_string())
            .filter(|m| !m.is_empty())
            .unwrap_or_else(|| "Could not read that playlist.".to_string());
        return Err(msg);
    }

    let json: serde_json::Value =
        serde_json::from_slice(&out.stdout).map_err(|_| "Could not parse the playlist.".to_string())?;

    let raw_entries = json
        .get("entries")
        .and_then(|e| e.as_array())
        .cloned()
        .unwrap_or_default();

    let mut entries: Vec<PlaylistEntry> = Vec::new();
    for (i, e) in raw_entries.iter().enumerate() {
        if e.is_null() {
            continue; // partially-available playlist
        }
        let title = pick(vec![s(e, "title"), s(e, "fulltitle"), format!("Video {}", i + 1)]);
        let id = pick(vec![s(e, "id"), s(e, "display_id")]);
        // With --flat-playlist the `url` field is the page URL, not media.
        let link = pick(vec![s(e, "webpage_url"), s(e, "original_url"), s(e, "url")]);
        if link.is_empty() {
            continue;
        }
        let thumbnail = e
            .get("thumbnails")
            .and_then(|t| t.as_array())
            .and_then(|arr| {
                arr.iter().rev().find_map(|t| {
                    t.get("url")
                        .and_then(|u| u.as_str())
                        .filter(|u| u.starts_with("http"))
                        .map(|u| u.to_string())
                })
            })
            .or_else(|| {
                e.get("thumbnail")
                    .and_then(|t| t.as_str())
                    .map(|t| t.to_string())
            })
            .unwrap_or_default();
        entries.push(PlaylistEntry {
            id,
            title,
            url: link,
            duration: opt_u64(e, "duration"),
            thumbnail,
            index: entries.len() + 1,
        });
    }

    if entries.is_empty() {
        return Err("That playlist has no downloadable videos (it may be private or empty).".to_string());
    }

    Ok(PlaylistInfo {
        title: pick(vec![s(&json, "title"), "Untitled playlist".to_string()]),
        uploader: pick(vec![s(&json, "uploader"), s(&json, "channel")]),
        count: entries.len(),
        thumbnail: json
            .get("thumbnails")
            .and_then(|t| t.as_array())
            .and_then(|arr| {
                arr.iter().rev().find_map(|t| {
                    t.get("url")
                        .and_then(|u| u.as_str())
                        .filter(|u| u.starts_with("http"))
                        .map(|u| u.to_string())
                })
            })
            .unwrap_or_default(),
        entries,
    })
}

#[tauri::command]
async fn fetch_info(
    app: AppHandle,
    url: String,
    cookies_from: Option<String>,
) -> Result<VideoInfo, String> {
    let url = url.trim().to_string();
    if url.is_empty() {
        return Err("Please paste a video URL first.".to_string());
    }
    let bin = match resolve_ytdlp(&app).await {
        Some(p) => p,
        None => PathBuf::from(ensure_ytdlp(app.clone()).await?),
    };

    let mut cmd = TokioCommand::new(&bin);
    hide_window(&mut cmd);
    cmd.args(["--no-playlist", "--no-warnings", "--socket-timeout", "25", "-J"]);
    if let Some(browser) = cookies_from.as_deref().filter(|b| !b.is_empty() && *b != "none") {
        cmd.args(["--cookies-from-browser", browser]);
    }
    cmd.arg(&url)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let out = tokio::time::timeout(Duration::from_secs(90), cmd.output())
        .await
        .map_err(|_| "Timed out while reading video info. Check your connection and retry.".to_string())?
        .map_err(|e| format!("Failed to run yt-dlp: {e}"))?;

    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        let msg = err
            .lines()
            .rev()
            .find(|l| l.contains("ERROR"))
            .map(|l| {
                l.split("ERROR:")
                    .nth(1)
                    .unwrap_or(l)
                    .trim()
                    .trim_start_matches('[')
                    .to_string()
            })
            .filter(|m| !m.is_empty())
            .unwrap_or_else(|| "Could not read that video.".to_string());
        return Err(msg);
    }

    let json: serde_json::Value =
        serde_json::from_slice(&out.stdout).map_err(|_| "Could not parse video info.".to_string())?;

    if json.get("_type").and_then(|t| t.as_str()) == Some("playlist") {
        let entries = json.get("entries").and_then(|e| e.as_array()).map(|a| a.len()).unwrap_or(0);
        if entries == 0 {
            return Err("That playlist is empty or private.".to_string());
        }
        return Err(format!(
            "That link is a playlist with {entries} videos. Open a single video/reel/post link \
             instead (paste the link of the item itself)."
        ));
    }

    let formats = json
        .get("formats")
        .and_then(|f| f.as_array())
        .cloned()
        .unwrap_or_default();

    let mut list: Vec<FormatInfo> = formats        .iter()
        .filter_map(|f| {
            let id = s(f, "format_id");
            if id.is_empty() {
                return None;
            }
            let vcodec = s(f, "vcodec");
            let acodec = s(f, "acodec");
            let has_video = !(vcodec == "none" || vcodec.is_empty());
            let has_audio = !(acodec == "none" || acodec.is_empty());
            if !has_video && !has_audio {
                return None; // storyboard / metadata entries
            }
            let ext = s(f, "ext");
            if ext == "mhtml" {
                return None;
            }
            let kind = if has_video && has_audio {
                "combined"
            } else if has_video {
                "video"
            } else {
                "audio"
            }
            .to_string();
            let filesize = opt_u64(f, "filesize").or_else(|| opt_u64(f, "filesize_approx"));
            Some(FormatInfo {
                label: build_label(f, has_video, has_audio),
                width: opt_u32(f, "width"),
                height: opt_u32(f, "height"),
                fps: opt_f64(f, "fps"),
                vcodec: codec_short(&vcodec),
                acodec: codec_short(&acodec),
                filesize,
                tbr: opt_f64(f, "tbr"),
                abr: opt_f64(f, "abr"),
                vbr: opt_f64(f, "vbr"),
                note: s(f, "format_note"),
                protocol: s(f, "protocol"),
                has_video,
                has_audio,
                kind,
                id,
                ext,
            })
        })
        .collect();

    // best quality first
    list.sort_by(|a, b| {
        b.height
            .unwrap_or(0)
            .cmp(&a.height.unwrap_or(0))
            .then(b.tbr.unwrap_or(0.0).partial_cmp(&a.tbr.unwrap_or(0.0)).unwrap())
            .then(b.filesize.unwrap_or(0).cmp(&a.filesize.unwrap_or(0)))
    });

    let thumbnail = json
        .get("thumbnails")
        .and_then(|t| t.as_array())
        .and_then(|arr| {
            arr.iter()
                .rev()
                .find_map(|t| t.get("url").and_then(|u| u.as_str()))
                .map(|u| u.to_string())
        })
        .or_else(|| {
            json.get("thumbnail")
                .and_then(|t| t.as_str())
                .map(|t| t.to_string())
        })
        .unwrap_or_default();

    let mut description = s(&json, "description");
    if description.len() > 1500 {
        description.truncate(1500);
        description.push('…');
    }

    let site = pick(vec![
        s(&json, "webpage_url_domain"),
        url.split("//")
            .nth(1)
            .unwrap_or("")
            .split('/')
            .next()
            .unwrap_or("")
            .trim_start_matches("www.")
            .to_string(),
    ]);
    let extractor = pick(vec![
        s(&json, "extractor_key"),
        s(&json, "extractor"),
        site.clone(),
    ]);

    if list.is_empty() {
        let needs_login = ["instagram", "facebook", "threads", "x", "twitter", "tiktok"]
            .iter()
            .any(|p| site.to_lowercase().contains(p));
        return Err(if needs_login {
            format!(
                "No downloadable streams were returned by {}. This site usually hides media \
                 unless you are logged in — try enabling browser cookies in Settings and retry.",
                if extractor.is_empty() { site.clone() } else { extractor.clone() }
            )
        } else {
            format!(
                "No downloadable formats were found for this link{}. The post may be private, \
                 deleted, DRM-protected, or the site may not be supported.",
                if extractor.is_empty() {
                    String::new()
                } else {
                    format!(" ({})", extractor)
                }
            )
        });
    }

    Ok(VideoInfo {
        id: s(&json, "id"),
        title: pick(vec![s(&json, "title"), "Untitled".to_string()]),
        uploader: pick(vec![s(&json, "uploader"), s(&json, "channel")]),
        channel: s(&json, "channel"),
        site,
        extractor,
        duration: opt_u64(&json, "duration"),
        views: opt_u64(&json, "view_count"),
        likes: opt_u64(&json, "like_count"),
        upload_date: s(&json, "upload_date"),
        description,
        thumbnail,
        url: pick(vec![s(&json, "webpage_url"), url]),
        format_count: list.len(),
        formats: list,
    })
}

// ---------------------------------------------------------------------------
// Downloads
// ---------------------------------------------------------------------------

struct DlState {
    children: std::sync::Arc<Mutex<HashMap<String, tokio::process::Child>>>,
    /// PIDs stay registered until the process actually exits, so Cancel can
    /// still reach a download that is in its wait/merge phase.
    pids: std::sync::Arc<Mutex<HashMap<String, u32>>>,
    cancelled: std::sync::Arc<Mutex<HashSet<String>>>,
}

#[derive(Deserialize)]
struct DlRequest {
    url: String,
    #[serde(rename = "formatSpec")]
    format_spec: String,
    #[serde(rename = "saveDir")]
    save_dir: String,
    #[serde(rename = "audioOnly")]
    audio_only: bool,
    #[serde(rename = "taskId")]
    task_id: String,
    #[serde(default)]
    fallbacks: Vec<String>,
    #[serde(default)]
    cookies_from: Option<String>,
}

#[derive(Deserialize)]
struct PlaylistDlRequest {
    urls: Vec<String>,
    #[serde(rename = "formatSpec")]
    format_spec: String,
    #[serde(rename = "saveDir")]
    save_dir: String,
    #[serde(rename = "audioOnly")]
    audio_only: bool,
    #[serde(rename = "taskId")]
    task_id: String,
    #[serde(default)]
    cookies_from: Option<String>,
}

#[derive(Serialize, Clone)]
struct DlEvent {
    #[serde(rename = "taskId")]
    task_id: String,
    kind: String, // progress | merging | info | done | error | cancelled
    percent: Option<f64>,
    speed: String,
    eta: String,
    downloaded: Option<u64>,
    total: Option<u64>,
    file: String,
    message: String,
}

fn parse_num(raw: &str) -> Option<u64> {
    let t = raw.trim();
    if t.is_empty() || t.eq_ignore_ascii_case("NA") {
        return None;
    }
    t.parse::<u64>().ok()
}

fn parse_percent(raw: &str) -> Option<f64> {
    let t = raw.trim().trim_end_matches('%').trim();
    if t.eq_ignore_ascii_case("NA") || t.is_empty() {
        return None;
    }
    t.parse::<f64>().ok()
}

fn clean_cell(raw: &str) -> String {
    let t = raw.trim().to_string();
    if t.eq_ignore_ascii_case("NA") || t.eq_ignore_ascii_case("unknown") {
        String::new()
    } else {
        t
    }
}

enum Attempt {
    Done(String),
    Failed(String),
    Cancelled,
}

/// Runs yt-dlp once for a single format spec. Progress/merging/log lines are
/// emitted live; hard errors are only recorded (the caller decides whether
/// to retry with an alternate spec or surface the failure).
#[allow(clippy::too_many_arguments)]
async fn run_attempt(
    app: &AppHandle,
    bin: &Path,
    args: &[String],
    task_id: &str,
    children: &std::sync::Arc<Mutex<HashMap<String, tokio::process::Child>>>,
    pids: &std::sync::Arc<Mutex<HashMap<String, u32>>>,
    cancelled: &std::sync::Arc<Mutex<std::collections::HashSet<String>>>,
) -> Attempt {
    let mut cmd = TokioCommand::new(bin);
    hide_window(&mut cmd);
    cmd.args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return Attempt::Failed(format!("Could not start download: {e}")),
    };
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    if let Some(pid) = child.id() {
        pids.lock().await.insert(task_id.to_string(), pid);
    }
    children.lock().await.insert(task_id.to_string(), child);

    let final_file = std::sync::Arc::new(Mutex::new(String::new()));
    let last_error = std::sync::Arc::new(Mutex::new(String::new()));

    let parse_line = |line: &str,
                      tid: &str,
                      emit: &dyn Fn(DlEvent),
                      final_file: &std::sync::Arc<Mutex<String>>,
                      last_error: &std::sync::Arc<Mutex<String>>| {
        let line = line.trim();
        if line.is_empty() {
            return;
        }
        if let Some(rest) = line.strip_prefix("DLPROG|") {
            let parts: Vec<&str> = rest.split('|').collect();
            let status = parts.first().copied().unwrap_or("");
            match status {
                "downloading" => {
                    let percent = parts.get(1).copied().unwrap_or("").pipe(parse_percent);
                    let speed = clean_cell(parts.get(2).copied().unwrap_or(""));
                    let eta = clean_cell(parts.get(3).copied().unwrap_or(""));
                    let downloaded = parts.get(4).copied().unwrap_or("").pipe(parse_num);
                    let total = parts.get(5).copied().unwrap_or("").pipe(parse_num);
                    emit(DlEvent {
                        task_id: tid.to_string(),
                        kind: "progress".into(),
                        percent,
                        speed,
                        eta,
                        downloaded,
                        total,
                        file: String::new(),
                        message: String::new(),
                    });
                }
                "finished" => {
                    emit(DlEvent {
                        task_id: tid.to_string(),
                        kind: "merging".into(),
                        percent: Some(100.0),
                        speed: String::new(),
                        eta: String::new(),
                        downloaded: None,
                        total: None,
                        file: String::new(),
                        message: "Finishing up…".into(),
                    });
                }
                _ => {}
            }
            return;
        }
        if line.starts_with("[download] Destination:") {
            let f = line.trim_start_matches("[download] Destination:").trim().to_string();
            if let Ok(mut g) = final_file.try_lock() {
                *g = f.clone();
            }
            emit(DlEvent {
                task_id: tid.to_string(),
                kind: "info".into(),
                percent: None,
                speed: String::new(),
                eta: String::new(),
                downloaded: None,
                total: None,
                file: f,
                message: String::new(),
            });
            return;
        }
        if line.starts_with("[Merger]")
            || line.starts_with("[ExtractAudio]")
            || line.starts_with("[VideoRemuxer]")
        {
            if let Some(q1) = line.find('"') {
                if let Some(q2) = line[q1 + 1..].find('"') {
                    let f = line[q1 + 1..q1 + 1 + q2].to_string();
                    if let Ok(mut g) = final_file.try_lock() {
                        *g = f.clone();
                    }
                    emit(DlEvent {
                        task_id: tid.to_string(),
                        kind: "merging".into(),
                        percent: None,
                        speed: String::new(),
                        eta: String::new(),
                        downloaded: None,
                        total: None,
                        file: f,
                        message: "Merging…".into(),
                    });
                    return;
                }
            }
            emit(DlEvent {
                task_id: tid.to_string(),
                kind: "merging".into(),
                percent: None,
                speed: String::new(),
                eta: String::new(),
                downloaded: None,
                total: None,
                file: String::new(),
                message: "Merging…".into(),
            });
            return;
        }
        if line.contains("has already been downloaded") {
            if let Some(end) = line.find(" has already been downloaded") {
                let start = line.find("] ").map(|i| i + 2).unwrap_or(0);
                let f = line[start..end].trim().to_string();
                if let Ok(mut g) = final_file.try_lock() {
                    if g.is_empty() {
                        *g = f;
                    }
                }
            }
            return;
        }
        if line.contains("ERROR") {
            let mut msg = line
                .split("ERROR:")
                .nth(1)
                .unwrap_or(line)
                .trim()
                .trim_start_matches('[')
                .to_string();
            if msg.len() > 300 {
                msg.truncate(300);
            }
            if let Ok(mut g) = last_error.try_lock() {
                *g = msg;
            }
            return;
        }
        // Forward remaining engine chatter (extraction steps, retries, …)
        // so the UI can show live status instead of a frozen spinner.
        if line.len() > 3 {
            let mut m = line.to_string();
            if m.len() > 160 {
                m.truncate(160);
                m.push('…');
            }
            emit(DlEvent {
                task_id: tid.to_string(),
                kind: "log".into(),
                percent: None,
                speed: String::new(),
                eta: String::new(),
                downloaded: None,
                total: None,
                file: String::new(),
                message: m,
            });
        }
    };

    let mut readers = Vec::new();
    if let Some(out) = stdout {
        let tid = task_id.to_string();
        let ff = final_file.clone();
        let le = last_error.clone();
        let app2 = app.clone();
        readers.push(tokio::spawn(async move {
            let mut lines = BufReader::new(out).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let emit2 = |ev: DlEvent| {
                    let _ = app2.emit("dl-event", ev);
                };
                parse_line(&line, &tid, &emit2, &ff, &le);
            }
        }));
    }
    if let Some(err) = stderr {
        let tid = task_id.to_string();
        let ff = final_file.clone();
        let le = last_error.clone();
        let app2 = app.clone();
        readers.push(tokio::spawn(async move {
            let mut lines = BufReader::new(err).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let emit2 = |ev: DlEvent| {
                    let _ = app2.emit("dl-event", ev);
                };
                parse_line(&line, &tid, &emit2, &ff, &le);
            }
        }));
    }

    // NOTE: never `await` while holding the children lock — Cancel needs that
    // lock to reach the child handle, so waiting inside the guard would
    // deadlock the UI's cancel button for the whole download. Instead we take
    // ownership of the child handle here, then wait on it lock-free.
    let taken: Option<tokio::process::Child> = children.lock().await.remove(task_id);
    let status: Result<std::process::ExitStatus, String> = match taken {
        Some(mut child) => child.wait().await.map_err(|e| e.to_string()),
        None => Err("cancelled".to_string()),
    };
    pids.lock().await.remove(task_id);

    for r in readers {
        let _ = r.await;
    }

    if cancelled.lock().await.contains(task_id) {
        return Attempt::Cancelled;
    }
    match status {
        Ok(st) if st.success() => {
            Attempt::Done(final_file.lock().await.clone())
        }
        Ok(st) => {
            let recorded = last_error.lock().await.clone();
            let msg = if recorded.is_empty() {
                format!("Download failed (exit code {})", st.code().unwrap_or(-1))
            } else {
                recorded
            };
            Attempt::Failed(msg)
        }
        Err(_) => Attempt::Cancelled,
    }
}

#[tauri::command]
async fn start_download(
    app: AppHandle,
    state: State<'_, DlState>,
    req: DlRequest,
) -> Result<serde_json::Value, String> {
    if req.url.trim().is_empty() {
        return Err("Missing video URL.".to_string());
    }
    if req.save_dir.trim().is_empty() {
        return Err("Choose a download folder first.".to_string());
    }
    let bin = match resolve_ytdlp(&app).await {
        Some(p) => p,
        None => PathBuf::from(ensure_ytdlp(app.clone()).await?),
    };

    let out_template = format!(
        "{}/%(title).120s [%(id)s].%(ext)s",
        req.save_dir.trim_end_matches(['/', '\\'])
    );

    // Attempt list: requested spec first, then alternates. YouTube
    // regularly 403s individual stream URLs and rotates format IDs, so a
    // failed attempt is retried with the next source automatically.
    let mut specs = vec![req.format_spec.clone()];
    for f in &req.fallbacks {
        if !specs.contains(f) {
            specs.push(f.clone());
        }
    }
    specs.truncate(4);

    let mut base_args: Vec<String> = vec![
        "--no-playlist".into(),
        "--windows-filenames".into(),
        "--no-warnings".into(),
        "--newline".into(),
        "--no-colors".into(),
        "--progress-template".into(),
        "DLPROG|%(progress.status)s|%(progress._percent_str)s|%(progress._speed_str)s|%(progress._eta_str)s|%(progress.downloaded_bytes)s|%(progress.total_bytes_estimate)s".into(),
    ];
    if let Some(browser) = req
        .cookies_from
        .as_deref()
        .filter(|b| !b.is_empty() && *b != "none")
    {
        base_args.push("--cookies-from-browser".into());
        base_args.push(browser.to_string());
    }
    let tail_args: Vec<String> = vec!["-o".into(), out_template];
    let mode_args: Vec<String> = if req.audio_only {
        vec![
            "-x".into(),
            "--audio-format".into(),
            "mp3".into(),
            "--audio-quality".into(),
            "0".into(),
        ]
    } else {
        vec!["--merge-output-format".into(), "mp4".into()]
    };

    let task_id = req.task_id.clone();
    let url = req.url.clone();
    let children = state.children.clone();
    let pids = state.pids.clone();
    let cancelled = state.cancelled.clone();
    let app_task = app.clone();
    let emit_start = |ev: DlEvent| {
        let _ = app_task.emit("dl-event", ev);
    };
    emit_start(DlEvent {
        task_id: task_id.clone(),
        kind: "info".into(),
        percent: Some(0.0),
        speed: String::new(),
        eta: String::new(),
        downloaded: None,
        total: None,
        file: String::new(),
        message: "Starting…".into(),
    });

    let task_id_ret = task_id.clone();
    tokio::spawn(async move {
        let total = specs.len();
        for (i, spec) in specs.iter().enumerate() {            if cancelled.lock().await.contains(&task_id) {
                let _ = app.emit(
                    "dl-event",
                    DlEvent {
                        task_id: task_id.clone(),
                        kind: "cancelled".into(),
                        percent: None,
                        speed: String::new(),
                        eta: String::new(),
                        downloaded: None,
                        total: None,
                        file: String::new(),
                        message: "Cancelled".into(),
                    },
                );
                return;
            }
            if i > 0 {
                let _ = app.emit(
                    "dl-event",
                    DlEvent {
                        task_id: task_id.clone(),
                        kind: "info".into(),
                        percent: None,
                        speed: String::new(),
                        eta: String::new(),
                        downloaded: None,
                        total: None,
                        file: String::new(),
                        message: format!(
                            "First source failed — trying alternate ({}/{})…",
                            i + 1,
                            total
                        ),
                    },
                );
            }
            let mut args = base_args.clone();
            args.push("-f".into());
            args.push(spec.clone());
            args.extend(tail_args.clone());
            args.extend(mode_args.clone());
            args.push(url.clone());

            match run_attempt(&app, &bin, &args, &task_id, &children, &pids, &cancelled).await {
                Attempt::Done(file) => {
                    let _ = app.emit(
                        "dl-event",
                        DlEvent {
                            task_id: task_id.clone(),
                            kind: "done".into(),
                            percent: Some(100.0),
                            speed: String::new(),
                            eta: String::new(),
                            downloaded: None,
                            total: None,
                            file: file.clone(),
                            message: if total > 1 && i > 0 {
                                "Download complete (alternate source)".into()
                            } else {
                                "Download complete".into()
                            },
                        },
                    );
                    return;
                }
                Attempt::Cancelled => {
                    let _ = app.emit(
                        "dl-event",
                        DlEvent {
                            task_id: task_id.clone(),
                            kind: "cancelled".into(),
                            percent: None,
                            speed: String::new(),
                            eta: String::new(),
                            downloaded: None,
                            total: None,
                            file: String::new(),
                            message: "Cancelled".into(),
                        },
                    );
                    return;
                }
                Attempt::Failed(msg) => {
                    if i + 1 >= total {
                        let _ = app.emit(
                            "dl-event",
                            DlEvent {
                                task_id: task_id.clone(),
                                kind: "error".into(),
                                percent: None,
                                speed: String::new(),
                                eta: String::new(),
                                downloaded: None,
                                total: None,
                                file: String::new(),
                                message: msg,
                            },
                        );
                        return;
                    }
                }
            }
        }
    });

    Ok(serde_json::json!({ "taskId": task_id_ret }))

    // (old single-attempt engine removed; see run_attempt above)

}

#[tauri::command]
async fn start_playlist_download(
    app: AppHandle,
    state: State<'_, DlState>,
    req: PlaylistDlRequest,
) -> Result<serde_json::Value, String> {
    if req.urls.iter().any(|u| u.trim().is_empty()) {
        return Err("Playlist contains an empty link.".to_string());
    }
    if req.save_dir.trim().is_empty() {
        return Err("Choose a download folder first.".to_string());
    }
    let total = req.urls.len();
    if total == 0 {
        return Err("No videos selected.".to_string());
    }
    let bin = match resolve_ytdlp(&app).await {
        Some(p) => p,
        None => PathBuf::from(ensure_ytdlp(app.clone()).await?),
    };

    let out_template = format!(
        "{}/%(title).120s [%(id)s].%(ext)s",
        req.save_dir.trim_end_matches(['/', '\\'])
    );

    let mut base_args: Vec<String> = vec![
        "--no-playlist".into(),
        "--windows-filenames".into(),
        "--no-warnings".into(),
        "--newline".into(),
        "--no-colors".into(),
        "--ignore-errors".into(),
        "--progress-template".into(),
        "DLPROG|%(progress.status)s|%(progress._percent_str)s|%(progress._speed_str)s|%(progress._eta_str)s|%(progress.downloaded_bytes)s|%(progress.total_bytes_estimate)s".into(),
        "-f".into(),
        req.format_spec.clone(),
        "-o".into(),
        out_template,
    ];
    if let Some(browser) = req
        .cookies_from
        .as_deref()
        .filter(|b| !b.is_empty() && *b != "none")
    {
        base_args.push("--cookies-from-browser".into());
        base_args.push(browser.to_string());
    }
    if req.audio_only {
        base_args.push("-x".into());
        base_args.push("--audio-format".into());
        base_args.push("mp3".into());
        base_args.push("--audio-quality".into());
        base_args.push("0".into());
    } else {
        base_args.push("--merge-output-format".into());
        base_args.push("mp4".into());
    }

    let task_id = req.task_id.clone();
    let urls = req.urls.clone();
    let children = state.children.clone();
    let pids = state.pids.clone();
    let cancelled = state.cancelled.clone();
    let emit_app = app.clone();
    let _ = emit_app.emit(
        "dl-event",
        DlEvent {
            task_id: task_id.clone(),
            kind: "info".into(),
            percent: Some(0.0),
            speed: String::new(),
            eta: String::new(),
            downloaded: None,
            total: None,
            file: String::new(),
            message: format!("Starting {total} videos…").into(),
        },
    );

    let task_id_ret = task_id.clone();
    let task_id_spawn = task_id.clone();
    // One yt-dlp process handles the whole batch sequentially, so bandwidth
    // and disk usage stay sane on large playlists.
    tokio::spawn(async move {
        // Shadow the outer handles: this closure owns its own copies.
        let task_id = task_id_spawn.clone();
        let mut args = base_args.clone();
        args.extend(urls.iter().cloned());

        match run_attempt(&app, &bin, &args, &task_id_spawn, &children, &pids, &cancelled).await {
            Attempt::Done(_) => {
                let _ = app.emit(
                    "dl-event",
                    DlEvent {
                        task_id: task_id.clone(),
                        kind: "done".into(),
                        percent: Some(100.0),
                        speed: String::new(),
                        eta: String::new(),
                        downloaded: None,
                        total: None,
                        file: String::new(),
                        message: format!("Playlist finished ({total} videos)").into(),
                    },
                );
            }
            Attempt::Cancelled => {
                let _ = app.emit(
                    "dl-event",
                    DlEvent {
                        task_id: task_id.clone(),
                        kind: "cancelled".into(),
                        percent: None,
                        speed: String::new(),
                        eta: String::new(),
                        downloaded: None,
                        total: None,
                        file: String::new(),
                        message: "Cancelled".into(),
                    },
                );
            }
            Attempt::Failed(msg) => {
                let _ = app.emit(
                    "dl-event",
                    DlEvent {
                        task_id: task_id.clone(),
                        kind: "error".into(),
                        percent: None,
                        speed: String::new(),
                        eta: String::new(),
                        downloaded: None,
                        total: None,
                        file: String::new(),
                        message: msg,
                    },
                );
            }
        }
    });

    Ok(serde_json::json!({ "taskId": task_id_ret }))
}

trait Pipe<T> {
    fn pipe<R>(self, f: fn(T) -> R) -> R;
}
impl<T> Pipe<T> for T {
    fn pipe<R>(self, f: fn(T) -> R) -> R {
        f(self)
    }
}

#[tauri::command]
async fn cancel_download(state: State<'_, DlState>, task_id: String) -> Result<(), String> {
    // Flag first so in-flight waits and pending retries observe it.
    state.cancelled.lock().await.insert(task_id.clone());
    // Kill on Windows needs the whole process tree (yt-dlp spawns ffmpeg),
    // otherwise a merge step can keep running after the parent is gone.
    #[cfg(windows)]
    let pid: Option<u32> = state.pids.lock().await.get(&task_id).copied();
    #[cfg(windows)]
    if let Some(pid) = pid {
        let _ = std::process::Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/T", "/F"])
            .creation_flags(0x08000000)
            .output();
    }
    #[cfg(not(windows))]
    {
        let mut guard = state.children.clone().lock_owned().await;
        if let Some(child) = guard.get_mut(&task_id) {
            let _ = child.kill().await;
        }
    }
    Ok(())
}

#[tauri::command]
async fn fetch_image(url: String) -> Result<String, String> {
    if url.trim().is_empty() {
        return Err("Empty image URL.".to_string());
    }
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(25))
        .user_agent("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0 Safari/537.36")
        .build()
        .map_err(|e| e.to_string())?;
    let resp = client
        .get(url.trim())
        .send()
        .await
        .map_err(|e| format!("Image download failed: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("Image download failed: HTTP {}", resp.status()));
    }
    let mime = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("image/jpeg")
        .to_string();
    let bytes = resp.bytes().await.map_err(|e| e.to_string())?;
    if bytes.len() > 8 * 1024 * 1024 {
        return Err("Image too large.".to_string());
    }
    use base64::Engine;
    Ok(format!(
        "data:{};base64,{}",
        mime,
        base64::engine::general_purpose::STANDARD.encode(&bytes)
    ))
}

// ---------------------------------------------------------------------------
// Misc helpers
// ---------------------------------------------------------------------------

#[tauri::command]
#[cfg_attr(
    any(target_os = "android", target_os = "ios"),
    allow(unused_variables)
)]
fn pick_folder(default: Option<String>) -> Result<Option<String>, String> {
    // Native folder picker is desktop-only; mobile uses a document picker.
    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    {
        let current = std::thread::spawn(move || {
            let mut dlg = rfd::FileDialog::new().set_title("Choose download folder");
            if let Some(d) = default {
                let p = PathBuf::from(d);
                if p.is_dir() {
                    dlg = dlg.set_directory(p);
                }
            }
            dlg.pick_folder().map(|p| p.to_string_lossy().to_string())
        })
        .join()
        .map_err(|_| "Folder dialog failed.".to_string())?;
        Ok(current)
    }
    #[cfg(any(target_os = "android", target_os = "ios"))]
    {
        let _ = default;
        Ok(None)
    }
}

#[tauri::command]
fn default_dir() -> Result<String, String> {
    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    {
        if let Some(v) = dirs::video_dir() {
            return Ok(v.to_string_lossy().to_string());
        }
        if let Some(d) = dirs::download_dir() {
            return Ok(d.to_string_lossy().to_string());
        }
    }
    // Mobile: let the user pick a destination instead of assuming a path.
    Ok(String::new())
}

#[tauri::command]
fn show_in_folder(path: String) -> Result<(), String> {
    let p = Path::new(&path);
    if !p.exists() {
        return Err("File not found on disk.".to_string());
    }
    #[cfg(windows)]
    {
        std::process::Command::new("explorer")
            .arg("/select,")
            .arg(path)
            .spawn()
            .map_err(|e| e.to_string())?;
        Ok(())
    }
    #[cfg(all(not(windows), not(any(target_os = "android", target_os = "ios"))))]
    {
        let parent = p
            .parent()
            .map(|x| x.to_string_lossy().to_string())
            .unwrap_or(path);
        open::that(parent).map_err(|e| e.to_string())
    }
    #[cfg(any(target_os = "android", target_os = "ios"))]
    {
        let _ = p;
        Ok(())
    }
}

/// Shared application wiring, used by both desktop and mobile entry points.
pub fn app() {
    tauri::Builder::default()
        .manage(DlState {
            children: std::sync::Arc::new(Mutex::new(HashMap::new())),
            pids: std::sync::Arc::new(Mutex::new(HashMap::new())),
            cancelled: std::sync::Arc::new(Mutex::new(HashSet::new())),
        })
        .invoke_handler(tauri::generate_handler![
            ytdlp_status,
            ensure_ytdlp,
            fetch_info,
            fetch_playlist,
            fetch_image,
            start_download,
            start_playlist_download,
            cancel_download,
            pick_folder,
            default_dir,
            show_in_folder
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
