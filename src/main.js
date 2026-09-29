/* YouTube Downloader frontend — vanilla JS on Tauri's injected __TAURI__ API */
const TAURI = window.__TAURI__;
if (!TAURI || !TAURI.core || !TAURI.event) {
  document.addEventListener("DOMContentLoaded", () => {
    const el = document.getElementById("fetchError");
    el.textContent = "App bridge not available — please restart the app. (window.__TAURI__ is missing)";
    el.classList.remove("hidden");
    document.getElementById("fetchBtn").disabled = true;
  });
  throw new Error("window.__TAURI__ is missing");
}
const { invoke } = TAURI.core;
const { listen } = TAURI.event;

const $ = (id) => document.getElementById(id);
const state = {
  video: null,
  filter: "all",
  sort: "quality",
  folder: localStorage.getItem("dlFolder") || "",
  cookies: localStorage.getItem("dlCookies") || "none",
  engineReady: false,
  downloads: new Map(), // taskId -> {title, spec, el refs..., status}
};

const QUICK_DEFS = {
  best: "Best quality",
  1080: "Max 1080p",
  720: "Max 720p",
  mp3: "MP3 audio",
};

/* ---------------- helpers ---------------- */
function toast(msg, kind = "") {
  const t = document.createElement("div");
  t.className = "toast " + kind;
  t.textContent = msg;
  $("toasts").appendChild(t);
  setTimeout(() => { t.style.opacity = "0"; t.style.transition = "opacity .3s"; }, 3600);
  setTimeout(() => t.remove(), 4000);
}

function fmtBytes(n) {
  if (n == null || isNaN(n)) return "—";
  const u = ["B", "KB", "MB", "GB"];
  let i = 0, v = n;
  while (v >= 1024 && i < u.length - 1) { v /= 1024; i++; }
  return (v >= 100 ? v.toFixed(0) : v.toFixed(1)) + " " + u[i];
}

function fmtSize(f) {
  if (f.filesize) return fmtBytes(f.filesize);
  if (f.tbr && state.video && state.video.duration) {
    return "~" + fmtBytes((f.tbr * 1000 / 8) * state.video.duration);
  }
  return "—";
}

function fmtDuration(sec) {
  if (sec == null) return "";
  sec = Math.round(sec);
  const h = Math.floor(sec / 3600), m = Math.floor((sec % 3600) / 60), s = sec % 60;
  return h > 0 ? `${h}:${String(m).padStart(2, "0")}:${String(s).padStart(2, "0")}` : `${m}:${String(s).padStart(2, "0")}`;
}

function fmtCount(n) {
  if (n == null) return "";
  if (n >= 1e9) return (n / 1e9).toFixed(1) + "B views";
  if (n >= 1e6) return (n / 1e6).toFixed(1) + "M views";
  if (n >= 1e3) return (n / 1e3).toFixed(1) + "K views";
  return n + " views";
}

function fmtDate(yyyymmdd) {
  if (!yyyymmdd || yyyymmdd.length !== 8) return "";
  return `${yyyymmdd.slice(0, 4)}-${yyyymmdd.slice(4, 6)}-${yyyymmdd.slice(6, 8)}`;
}

function esc(s) {
  return String(s ?? "").replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c]));
}

function setEngine(mode, text) {
  const pill = $("enginePill");
  pill.className = "pill " + mode;
  $("engineText").textContent = text;
}

/* ---------------- engine ---------------- */
async function checkEngine() {
  try {
    const st = await invoke("ytdlp_status");
    if (st.available) {
      state.engineReady = true;
      setEngine("ready", "Engine ready" + (st.version ? " · v" + st.version : ""));
      $("setupBanner").classList.add("hidden");
    } else {
      state.engineReady = false;
      setEngine("missing", "Engine missing");
      $("setupBanner").classList.remove("hidden");
    }
  } catch (e) {
    setEngine("missing", "Engine check failed");
  }
}

async function installEngine() {
  $("setupBtn").disabled = true;
  $("setupProgress").classList.remove("hidden");
  setEngine("working", "Installing engine…");
  try {
    await invoke("ensure_ytdlp");
    state.engineReady = true;
    setEngine("ready", "Engine ready");
    $("setupBanner").classList.add("hidden");
    toast("Engine installed — you can fetch formats now.", "ok");
  } catch (e) {
    setEngine("missing", "Engine missing");
    toast("Engine install failed: " + e, "err");
    $("setupBtn").disabled = false;
  }
}

/* ---------------- folder ---------------- */
async function initFolder() {
  if (!state.folder) {
    try { state.folder = await invoke("default_dir"); }
    catch { state.folder = ""; }
    if (state.folder) localStorage.setItem("dlFolder", state.folder);
  }
  renderFolder();
}
function renderFolder() {
  $("folderPath").textContent = state.folder || "No folder selected";
}
async function changeFolder() {
  try {
    const picked = await invoke("pick_folder", { default: state.folder || null });
    if (picked) {
      state.folder = picked;
      localStorage.setItem("dlFolder", picked);
      renderFolder();
      toast("Download folder updated.", "ok");
    }
  } catch (e) { toast(String(e), "err"); }
}

/* ---------------- fetch ---------------- */
function validUrl(u) {
  const t = u.trim();
  return /^https?:\/\/[^\s/$.?#][^\s]*$/i.test(t) || /^[^\s/$.?#][^\s]*\.[a-z]{2,}(\/\S*)?$/i.test(t);
}

async function fetchFormats() {
  const url = $("urlInput").value.trim();
  $("fetchError").classList.add("hidden");
  if (!url) { showError("Paste a video URL first."); return; }
  if (!validUrl(url)) { showError("That doesn't look like a valid link. Paste a full URL (https://…)."); return; }

  $("fetchBtn").disabled = true;
  $("fetchLabel").textContent = "Loading…";
  $("fetchSkeleton").classList.remove("hidden");
  ["videoCard", "quickCard", "formatsCard"].forEach((id) => $(id).classList.add("hidden"));

  try {
    // Playlists are handled by their own view (expand + select), so probe for
    // one first and fall back to single-video formats.
    if (looksLikePlaylist(url)) {
      const pl = await invoke("fetch_playlist", { url, cookiesFrom: state.cookies || null });
      state.video = null;
      ["videoCard", "quickCard", "formatsCard"].forEach((id) => $(id).classList.add("hidden"));
      renderPlaylist(pl);
      return;
    }
    const info = await invoke("fetch_info", { url, cookiesFrom: state.cookies || null });
    state.video = info;
    $("plCard").classList.add("hidden");
    renderVideo(info);
    renderFormats();
  } catch (e) {
    showError(String(e));
  } finally {
    $("fetchBtn").disabled = false;
    $("fetchLabel").textContent = "Get formats";
    $("fetchSkeleton").classList.add("hidden");
  }
}
function showError(msg) {
  const el = $("fetchError");
  el.textContent = msg;
  el.classList.remove("hidden");
}

function renderVideo(v) {
  $("vThumb").removeAttribute("src");
  $("vTitle").textContent = v.title;
  const siteEl = $("vSite");
  if (v.extractor || v.site) {
    siteEl.textContent = v.extractor || v.site;
    siteEl.classList.remove("hidden");
  } else {
    siteEl.classList.add("hidden");
  }
  $("vUploader").textContent = v.uploader || v.channel || "";
  $("vDuration").textContent = v.duration != null ? "⏱ " + fmtDuration(v.duration) : "";
  $("vDuration").style.display = v.duration != null ? "" : "none";
  $("vViews").textContent = v.views != null ? "👁 " + fmtCount(v.views) : "";
  $("vViews").style.display = v.views != null ? "" : "none";
  $("vDate").textContent = v.upload_date ? "📅 " + fmtDate(v.upload_date) : "";
  $("vDate").style.display = v.upload_date ? "" : "none";
  $("vDesc").textContent = v.description || "";
  $("vDesc").style.display = v.description ? "" : "none";
  $("videoCard").classList.remove("hidden");
  $("quickCard").classList.remove("hidden");
  $("formatsCard").classList.remove("hidden");
  // Load thumbnails through the backend so the WebView never talks to
  // third-party image hosts directly (avoids tracking-prevention blocks).
  const img = $("vThumb");
  if (v.thumbnail) {
    invoke("fetch_image", { url: v.thumbnail })
      .then((dataUrl) => { img.src = dataUrl; })
      .catch(() => { img.src = v.thumbnail; });
  }
}

/* ---------------- playlist ---------------- */
function plSpec(key) {
  switch (key) {
    case "1080": return "bv*[height<=1080]+ba/b[height<=1080]/b[height<=1080]/b";
    case "720": return "bv*[height<=720]+ba/b[height<=720]/b[height<=720]/b";
    case "mp3": return "bestaudio/best";
    default: return "bv*+ba/b";
  }
}

function renderPlaylist(pl) {
  state.playlist = pl;
  $("plTitle").textContent = pl.title;
  $("plMeta").textContent = `${pl.count} videos${pl.uploader ? " · " + pl.uploader : ""}`;
  $("plList").innerHTML = "";
  // Default: everything selected, per your request.
  for (const e of pl.entries) {
    const row = document.createElement("label");
    row.className = "pl-item sel";
    row.innerHTML = `
      <input type="checkbox" checked />
      <span class="pl-num">${e.index}</span>
      <img alt="" />
      <span class="pl-t">${esc(e.title)}</span>
      <span class="pl-d">${e.duration != null ? esc(fmtDuration(e.duration)) : ""}</span>`;
    if (e.thumbnail) {
      invoke("fetch_image", { url: e.thumbnail })
        .then((d) => { row.querySelector("img").src = d; })
        .catch(() => { row.querySelector("img").src = e.thumbnail; });
    }
    const cb = row.querySelector("input");
    cb.addEventListener("change", () => {
      row.classList.toggle("sel", cb.checked);
      updatePlSelected();
    });
    row.addEventListener("click", (ev) => {
      if (ev.target !== cb) { cb.checked = !cb.checked; row.classList.toggle("sel", cb.checked); updatePlSelected(); }
    });
    row.dataset.url = e.url;
    $("plList").appendChild(row);
  }
  $("plCard").classList.remove("hidden");
  $("plList").classList.add("hidden");
  $("plExpand").textContent = "▾ Show videos";
  updatePlSelected();
}

function selectedUrls() {
  return [...$("plList").querySelectorAll("input:checked")].map((cb) => cb.closest(".pl-item").dataset.url);
}

function updatePlSelected() {
  const n = selectedUrls().length;
  $("plSelected").textContent = `${n} of ${state.playlist?.entries.length ?? 0} selected`;
  $("plDownload").disabled = n === 0;
  $("plDownload").textContent = n === 0 ? "Download selected" : `Download ${n} video${n > 1 ? "s" : ""}`;
  const all = document.querySelectorAll("#plList input").length;
  $("plToggleAll").textContent = n === all ? "Deselect all" : "Select all";
}

async function startPlaylistDownload() {
  if (!ensureFolder()) return;
  const urls = selectedUrls();
  if (!urls.length) return toast("Select at least one video.", "err");
  const key = $("plQuality").value;
  const audioOnly = key === "mp3";
  const taskId = crypto.randomUUID();
  addDlCard(taskId, `Playlist · ${state.playlist.title}`, `${urls.length} videos · ${key === "mp3" ? "MP3" : key === "best" ? "best quality" : "max " + key + "p"}`);
  try {
    await invoke("start_playlist_download", {
      req: {
        urls,
        formatSpec: plSpec(key),
        saveDir: state.folder,
        audioOnly,
        taskId,
        cookiesFrom: state.cookies || null,
      },
    });
  } catch (e) {
    updateDl(taskId, { status: "err", msg: String(e) });
    toast("Could not start playlist download: " + e, "err");
  }
}

/* ---------------- formats table ---------------- */
function bestAudio() {
  const audios = (state.video?.formats ?? []).filter((f) => f.kind === "audio");
  audios.sort((a, b) => (b.abr || 0) - (a.abr || 0) || (b.filesize || 0) - (a.filesize || 0));
  return audios[0] || null;
}

// YouTube rarely ships single-file video+audio streams above 720p.
// For the "Video + audio" tab we synthesize one-click rows that merge
// each video-only quality with the best audio track into a single MP4.
function mergedRows() {
  const ba = bestAudio();
  return (state.video?.formats ?? [])
    .filter((f) => f.kind === "video")
    .map((f) => ({
      kind: "merged",
      id: `${f.id}+bestaudio`,
      label: f.label.replace(" (no audio)", ""),
      ext: "mp4",
      width: f.width,
      height: f.height,
      fps: f.fps,
      vcodec: f.vcodec,
      acodec: ba ? ba.acodec : "best",
      filesize: f.filesize && ba && ba.filesize ? f.filesize + ba.filesize : null,
      tbr: (f.tbr || 0) + (ba && ba.tbr ? ba.tbr : 0) || null,
      note: "merged on download",
      protocol: f.protocol,
      has_video: true,
      has_audio: true,
    }));
}

function filteredFormats() {
  const all = state.video?.formats ?? [];
  let list;
  if (state.filter === "all") list = [...all];
  else if (state.filter === "combined")
    list = [...mergedRows(), ...all.filter((f) => f.kind === "combined")];
  else list = all.filter((f) => f.kind === state.filter);
  if (state.sort === "size-desc") list = [...list].sort((a, b) => (b.filesize || 0) - (a.filesize || 0));
  else if (state.sort === "size-asc") list = [...list].sort((a, b) => (a.filesize || Infinity) - (b.filesize || Infinity));
  return list;
}

// Alternate specs to try automatically if YouTube 403s a stream URL or a
// format ID has rotated away since the list was fetched.
function fallbacksFor(f) {
  const all = state.video?.formats ?? [];
  const sib = (kind) =>
    all.filter((o) => o.kind === kind && o.id !== f.id && (o.height || 0) === (f.height || 0)).slice(0, 2);
  if (f.kind === "merged") {
    const fb = sib("video").map((o) => `${o.id}+bestaudio`);
    if (f.height) fb.push(`bv*[height<=${f.height}]+ba/b`);
    return fb.slice(0, 3);
  }
  if (f.kind === "combined") return ["b"];
  if (f.kind === "video") {
    const fb = sib("video").map((o) => o.id);
    if (f.height) fb.push(`bv*[height<=${f.height}]`);
    return fb.slice(0, 3);
  }
  return ["bestaudio"];
}

function specFor(f) {
  if (f.kind === "merged") return { spec: f.id, audioOnly: false, label: `${f.label} + best audio · MP4` };
  if (f.kind === "combined") return { spec: f.id, audioOnly: false, label: `${f.label} · ${f.ext}` };
  if (f.kind === "video") return { spec: f.id, audioOnly: false, label: `${f.label} video only · ${f.ext}` };
  return { spec: f.id, audioOnly: false, label: `${f.label} audio · ${f.ext}` };
}

function renderFormats() {
  const list = filteredFormats();
  $("fmtCount").textContent = `${state.video.formats.length} found`;
  const body = $("fmtBody");
  body.innerHTML = "";
  if (!list.length) {
    body.innerHTML = `<tr><td colspan="8" class="dim" style="text-align:center;padding:24px">No formats in this category.</td></tr>`;
    return;
  }
  list.forEach((f, i) => {
    const tr = document.createElement("tr");
    if (i === 0) tr.className = "hero";
    const typeLabel = f.kind === "video" ? "Video only" : f.kind === "audio" ? "Audio only" : "Video + audio";
    tr.innerHTML = `
      <td><span class="q-badge">${esc(f.label)}</span>${f.note ? `<div class="q-note">${esc(f.note)}</div>` : ""}</td>
      <td><span class="type ${f.kind}">${typeLabel}</span></td>
      <td class="mono">.${esc(f.ext)}</td>
      <td class="mono">${esc(f.vcodec)}</td>
      <td class="mono">${esc(f.acodec)}</td>
      <td class="mono">${f.fps ? Math.round(f.fps) : "—"}</td>
      <td class="mono">${esc(fmtSize(f))}</td>
      <td><button class="btn primary small dl-btn">Download</button></td>`;
    tr.querySelector(".dl-btn").addEventListener("click", () => {
      const s = specFor(f);
      startDownload(s.spec, s.audioOnly, s.label, fallbacksFor(f));
    });
    body.appendChild(tr);
  });
}

/* Quick-download presets. yt-dlp's `bv*`/`ba` selectors are YouTube-specific, so
   for other sites we fall back to the generic `bv*+ba/b` equivalents. */
function looksLikePlaylist(u) {
  return /[?&]list=|[?&]album=|\/playlist(\?|$)|\/videos(\?|$)|music\.youtube\.com\/playlist/i.test(u);
}

function isYouTube() {
  const e = (state.video?.extractor || state.video?.site || "").toLowerCase();
  return e.includes("youtube");
}

function quickSpec(key) {
  const yt = isYouTube();
  switch (key) {
    case "best":
      return yt
        ? { spec: "bv*+ba/b", audioOnly: false, label: "Best quality", fb: ["b"] }
        : { spec: "bv*+ba/b", audioOnly: false, label: "Best quality", fb: ["b"] };
    case "1080":
      return {
        spec: "bv*[height<=1080]+ba/b[height<=1080]/b[height<=1080]/b",
        audioOnly: false,
        label: "Max 1080p",
        fb: ["bv*[height<=1080]+ba/b", "b"],
      };
    case "720":
      return {
        spec: "bv*[height<=720]+ba/b[height<=720]/b[height<=720]/b",
        audioOnly: false,
        label: "Max 720p",
        fb: ["bv*[height<=720]+ba/b", "b"],
      };
    case "mp3":
      return { spec: "bestaudio/best", audioOnly: true, label: "MP3 audio", fb: ["bestaudio"] };
    default:
      return { spec: "b", audioOnly: false, label: "Best", fb: [] };
  }
}
/* ---------------- downloads ---------------- */
function ensureFolder() {
  if (!state.folder) {
    toast("Choose a download folder first (bottom of the page).", "err");
    return false;
  }
  return true;
}

async function startDownload(spec, audioOnly, label, fallbacks = []) {
  if (!state.video || !ensureFolder()) return;
  const taskId = crypto.randomUUID();
  const title = state.video.title;
  addDlCard(taskId, title, label || spec);
  try {
    await invoke("start_download", {
      req: {
        url: state.video.url,
        formatSpec: spec,
        saveDir: state.folder,
        audioOnly,
        taskId,
        fallbacks,
        cookiesFrom: state.cookies || null,
      },
    });
  } catch (e) {
    updateDl(taskId, { status: "err", msg: String(e) });
    toast("Could not start download: " + e, "err");
  }
}

function addDlCard(taskId, title, sub) {
  $("dlEmpty").style.display = "none";
  const el = document.createElement("div");
  el.className = "dl-item";
  el.innerHTML = `
    <div class="dl-top">
      <div style="min-width:0"><div class="dl-title">${esc(title)}</div><div class="dl-sub">${esc(sub)}</div></div>
      <div class="dl-status wait">Starting…</div>
    </div>
    <div class="bar"><div class="fill"></div></div>
    <div class="dl-meta">
      <div class="dl-stats">Preparing…</div>
      <div class="dl-actions">
        <button class="btn small ghost act-reveal hidden">Show file</button>
        <button class="btn small ghost act-cancel">Cancel</button>
      </div>
    </div>`;
  $("dlList").prepend(el);
  const rec = {
    title, file: "",
    statusEl: el.querySelector(".dl-status"),
    fillEl: el.querySelector(".fill"),
    statsEl: el.querySelector(".dl-stats"),
    revealBtn: el.querySelector(".act-reveal"),
    cancelBtn: el.querySelector(".act-cancel"),
    itemEl: el, status: "run",
  };
  rec.cancelBtn.addEventListener("click", async () => {
    rec.cancelBtn.disabled = true;
    try { await invoke("cancel_download", { taskId }); }
    catch (e) { toast(String(e), "err"); rec.cancelBtn.disabled = false; }
  });
  rec.revealBtn.addEventListener("click", async () => {
    try { await invoke("show_in_folder", { path: rec.file }); }
    catch (e) { toast(String(e), "err"); }
  });
  state.downloads.set(taskId, rec);
  updateDlCount();
}

function updateDl(taskId, { status, percent, speed, eta, downloaded, total, file, msg }) {
  const rec = state.downloads.get(taskId);
  if (!rec) return;
  if (file) rec.file = file;
  // live engine chatter: show it in the stats line so cards never look frozen
  if (status === "info" || status === "log") {
    if (msg) rec.statsEl.textContent = msg.length > 140 ? msg.slice(0, 140) + "…" : msg;
    else if (file) rec.statsEl.textContent = "Saving: " + file.split(/[\\/]/).pop();
    if (status === "info" && rec.statusEl.textContent === "Starting…") {
      rec.statusEl.textContent = "Working…";
      rec.statusEl.className = "dl-status run";
    }
    return;
  }
  const fmtPair = (downloaded != null || total != null)
    ? `${downloaded != null ? fmtBytes(downloaded) : "?"} / ${total != null ? fmtBytes(total) : "?"}`
    : "";
  if (status === "progress") {    rec.statusEl.textContent = percent != null ? percent.toFixed(1) + "%" : "Downloading…";
    rec.statusEl.className = "dl-status run";
    if (percent != null) rec.fillEl.style.width = Math.min(100, percent) + "%";
    rec.statsEl.textContent = [fmtPair, speed, eta ? "ETA " + eta : ""].filter(Boolean).join(" · ") || "Downloading…";
  } else if (status === "merging") {
    rec.statusEl.textContent = "Merging…";
    rec.statusEl.className = "dl-status run";
    rec.fillEl.style.width = "100%";
    rec.statsEl.textContent = msg || "Merging streams…";
  } else if (status === "done") {
    rec.status = "done";
    rec.itemEl.classList.add("done");
    rec.statusEl.textContent = "Done ✓";
    rec.statusEl.className = "dl-status done";
    rec.fillEl.style.width = "100%";
    rec.statsEl.textContent = rec.file || "Download complete";
    rec.cancelBtn.remove();
    if (rec.file) rec.revealBtn.classList.remove("hidden");
    toast(`Finished: ${rec.title}`, "ok");
  } else if (status === "err") {
    rec.status = "err";
    rec.itemEl.classList.add("error");
    rec.statusEl.textContent = "Failed";
    rec.statusEl.className = "dl-status err";
    rec.statsEl.textContent = msg || "Download failed";
    rec.cancelBtn.remove();
    toast("Download failed: " + (msg || "unknown error"), "err");
  } else if (status === "cancelled") {
    rec.status = "cancelled";
    rec.statusEl.textContent = "Cancelled";
    rec.statusEl.className = "dl-status wait";
    rec.statsEl.textContent = "Cancelled by user";
    rec.cancelBtn.remove();
  }
  updateDlCount();
}

function updateDlCount() {
  const active = [...state.downloads.values()].filter((d) => d.status === "run").length;
  const el = $("dlCount");
  el.textContent = active > 0 ? `${active} active` : `${state.downloads.size} total`;
  el.classList.toggle("hidden", state.downloads.size === 0);
}

async function bindDlEvents() {
  await listen("dl-event", (ev) => {
    const p = ev.payload;
    updateDl(p.taskId, {
      status: p.kind, percent: p.percent, speed: p.speed, eta: p.eta,
      downloaded: p.downloaded, total: p.total, file: p.file, msg: p.message,
    });
  });
  await listen("ytdlp-setup", (ev) => {
    const { percent, message } = ev.payload;
    $("setupMsg").textContent = message;
    if (percent != null) $("setupBar").style.width = percent + "%";
  });
}

/* ---------------- wiring ---------------- */
function init() {
  $("fetchBtn").addEventListener("click", fetchFormats);
  $("urlInput").addEventListener("keydown", (e) => { if (e.key === "Enter") fetchFormats(); });
  $("pasteBtn").addEventListener("click", async () => {
    try {
      const t = await navigator.clipboard.readText();
      if (t) { $("urlInput").value = t.trim(); $("urlInput").focus(); }
      else toast("Clipboard is empty.", "err");
    } catch { toast("Clipboard blocked — paste with Ctrl+V.", "err"); }
  });
  $("setupBtn").addEventListener("click", installEngine);
  $("plExpand").addEventListener("click", () => {
    const list = $("plList");
    const hidden = list.classList.toggle("hidden");
    $("plExpand").textContent = hidden ? "▾ Show videos" : "▴ Hide videos";
  });
  $("plToggleAll").addEventListener("click", () => {
    const boxes = [...$("plList").querySelectorAll("input")];
    const allChecked = boxes.every((b) => b.checked);
    boxes.forEach((b) => {
      b.checked = !allChecked;
      b.closest(".pl-item").classList.toggle("sel", b.checked);
    });
    updatePlSelected();
  });
  $("plDownload").addEventListener("click", startPlaylistDownload);
  $("changeFolderBtn").addEventListener("click", changeFolder);
  const cookieSel = $("cookieSel");
  cookieSel.value = state.cookies;
  cookieSel.addEventListener("change", () => {
    state.cookies = cookieSel.value;
    localStorage.setItem("dlCookies", state.cookies);
    toast(state.cookies === "none" ? "Cookies off." : `Using ${cookieSel.selectedOptions[0].text} cookies.`, "ok");
  });
  $("openFolderBtn").addEventListener("click", async () => {
    if (!state.folder) return toast("No folder selected yet.", "err");
    try { await invoke("show_in_folder", { path: state.folder }); }
    catch (e) { toast(String(e), "err"); }
  });
  $("openOrigBtn").addEventListener("click", () => {
    if (state.video?.url) {
      const a = document.createElement("a");
      a.href = state.video.url; a.target = "_blank"; a.rel = "noopener";
      a.click();
    }
  });
  $("copyLinkBtn").addEventListener("click", async () => {
    try { await navigator.clipboard.writeText(state.video.url); toast("Link copied.", "ok"); }
    catch { toast("Copy blocked by the browser.", "err"); }
  });
  document.querySelectorAll(".tab").forEach((t) =>
    t.addEventListener("click", () => {
      document.querySelectorAll(".tab").forEach((x) => x.classList.remove("active"));
      t.classList.add("active");
      state.filter = t.dataset.f;
      renderFormats();
    })
  );
  $("sortSel").addEventListener("change", (e) => { state.sort = e.target.value; renderFormats(); });
  document.querySelectorAll(".quick").forEach((b) =>
    b.addEventListener("click", () => {
      const q = quickSpec(b.dataset.q);
      startDownload(q.spec, q.audioOnly, q.label, q.fb || []);
    })
  );
  $("clearDoneBtn").addEventListener("click", () => {
    for (const [id, d] of state.downloads) {
      if (d.status !== "run") { d.itemEl.remove(); state.downloads.delete(id); }
    }
    if (state.downloads.size === 0) $("dlEmpty").style.display = "";
    updateDlCount();
  });

  bindDlEvents().catch(console.error);
  checkEngine();
  initFolder();
}

document.addEventListener("DOMContentLoaded", init);
