// GPW Uploader - orquestracao da UI.
// Fluxo: login (Supabase) -> arrasta pasta -> scan -> analise BPM/Key ->
// conversao MP3 dos masters -> "Continuar upload": sobe os arquivos como
// rascunho no site e abre upload.html?edit=<id> (campos pre-preenchidos,
// arquivos ja enviados) para o produtor finalizar no site.
//
// Usa a API global do Tauri (withGlobalTauri: true).

import { renderScan } from "./scanner-ui.js";
import { runQc } from "./qc.js";
import { loadDefaults, saveDefaults, GENRES } from "./config.js";
import { login, loadSession, currentSession, clearSession, getValidToken } from "./supabase.js";

const { invoke } = window.__TAURI__.core;
const { getCurrentWebview } = window.__TAURI__.webview;
const { listen } = window.__TAURI__.event;

// Abre uma URL no navegador padrao (plugin opener). A API global pode expor
// openUrl (v2) ou open; tentamos os dois.
function openExternal(url) {
  const op = window.__TAURI__.opener;
  if (op?.openUrl) return op.openUrl(url);
  if (op?.open) return op.open(url);
  return Promise.reject(new Error("opener plugin unavailable"));
}

const LAST_FOLDER_KEY = "gpwUploader.lastFolder";
const GPW_BASE = "https://ghostproducerworld.com";

const els = {};
const state = {
  session: null,
  profile: null,
  defaults: loadDefaults(),
  scan: null,
  analysis: null,
  converted: [],
  convRows: {},
  upRows: {},
  uploading: false,
  lastFolder: "",
  // Rascunho ja criado no site (Retry retoma em vez de duplicar) + campos
  // cujo arquivo ja subiu com sucesso (nao re-envia).
  draftId: null,
  uploadedFields: new Set(),
  cancelRequested: false,
};

// Id da rodada de scan atual. Cada handleDrop incrementa; a analise e a
// conversao da rodada anterior (async ainda em curso) descartam o resultado
// se o id mudou — senao os MP3s/BPM de uma pasta antiga entram na track nova.
let scanRun = 0;

// Masters -> MP3 gerado (so os masters Extended/Radio viram MP3).
const MP3_MAP = {
  extended_mix: { field: "xf_extended_mp3", out_name: "ExtendedMaster.mp3", label: "Extended Mix (MP3)" },
  radio_mix: { field: "xf_radio_mp3", out_name: "RadioMaster.mp3", label: "Radio Mix (MP3)" },
};

function el(tag, className, text) {
  const node = document.createElement(tag);
  if (className) node.className = className;
  if (text != null) node.textContent = text;
  return node;
}

function formatSize(bytes) {
  if (!bytes) return "";
  const mb = bytes / (1024 * 1024);
  return mb >= 1 ? `${mb.toFixed(1)} MB` : `${Math.max(1, Math.round(bytes / 1024))} KB`;
}

function cacheEls() {
  const id = (x) => document.getElementById(x);
  els.status = id("status");
  els.version = id("version");
  els.updateBar = id("update-bar");
  els.updateText = id("update-text");
  els.updateBtn = id("update-btn");
  els.updateDismiss = id("update-dismiss");
  // account / nav
  els.account = id("account");
  els.accountAvatar = id("account-avatar");
  els.accountEmail = id("account-email");
  els.settingsBtn = id("settings-btn");
  els.logoutBtn = id("logout-btn");
  // login
  els.login = id("login");
  els.loginForm = id("login-form");
  els.loginEmail = id("login-email");
  els.loginPassword = id("login-password");
  els.loginBtn = id("login-btn");
  els.loginError = id("login-error");
  // settings
  els.settings = id("settings");
  els.settingsClose = id("settings-close");
  els.settingsSave = id("settings-save");
  els.defOs = id("def-os");
  els.defDaw = id("def-daw");
  els.defDawVersion = id("def-daw-version");
  els.defPlugins = id("def-plugins");
  els.defHardware = id("def-hardware");
  els.defPrice = id("def-price");
  els.defGenre = id("def-genre");
  els.defProjectSale = id("def-project-sale");
  // Popula o dropdown de generos (vazio = sem padrao).
  if (els.defGenre && !els.defGenre.options.length) {
    els.defGenre.appendChild(el("option", null, "No default (pick per track)")).value = "";
    for (const g of GENRES) {
      const opt = el("option", null, g);
      opt.value = g;
      els.defGenre.appendChild(opt);
    }
  }
  // dropzone
  els.dropzone = id("dropzone");
  els.browseBtn = id("browse-btn");
  els.dropzoneLast = id("dropzone-last");
  els.dropzoneLastFolder = id("dropzone-last-folder");
  els.rescanBtn = id("rescan-btn");
  // results
  els.results = id("results");
  els.resultsPath = id("results-path");
  els.resultsSummary = id("results-summary");
  els.resultsWarnings = id("results-warnings");
  els.resultsList = id("results-list");
  els.resetBtn = id("reset-btn");
  els.continueBtn = id("continue-btn");
  els.cancelBtn = id("cancel-btn");
  els.continueStatus = id("continue-status");
  els.uploadProgress = id("upload-progress");
  els.continueResult = id("continue-result");
  // analysis
  els.analysis = id("analysis");
  els.anBpm = id("an-bpm");
  els.anKey = id("an-key");
  els.anCamelot = id("an-camelot");
  els.anStatus = id("an-status");
  // conversion
  els.conversion = id("conversion");
  els.conversionList = id("conversion-list");
}

function show(section) {
  for (const s of [els.login, els.settings, els.dropzone, els.results]) {
    s.classList.add("hidden");
  }
  section.classList.remove("hidden");
}

// ---- Healthcheck -----------------------------------------------------------
async function healthcheck() {
  try {
    const res = await invoke("ping");
    els.status.textContent = "backend ready";
    els.status.className = "status status--ok";
    els.version.textContent = `v${res.version}`;
  } catch (err) {
    els.status.textContent = "backend unavailable";
    els.status.className = "status status--error";
    console.error("ping falhou:", err);
  }
}

// ---- Auto-update (Tauri updater) -------------------------------------------
let _pendingUpdate = null;

// Verifica no GitHub Releases se ha uma versao nova (silencioso se nao houver).
async function checkForUpdates() {
  try {
    const updater = window.__TAURI__.updater;
    if (!updater?.check) return;
    const update = await updater.check();
    if (update && (update.available ?? true)) {
      _pendingUpdate = update;
      els.updateText.textContent = `Version ${update.version} is available.`;
      els.updateBar.classList.remove("hidden");
    }
  } catch (err) {
    console.warn("update check failed:", err);
  }
}

// Baixa, instala e reinicia o app.
async function doUpdate() {
  if (!_pendingUpdate) return;
  els.updateBtn.disabled = true;
  els.updateDismiss.disabled = true;
  els.updateText.textContent = "Downloading update…";
  try {
    await _pendingUpdate.downloadAndInstall();
    els.updateText.textContent = "Restarting…";
    const proc = window.__TAURI__.process;
    if (proc?.relaunch) await proc.relaunch();
  } catch (err) {
    els.updateText.textContent = "Update failed — try again later.";
    els.updateBtn.disabled = false;
    els.updateDismiss.disabled = false;
    console.error("update failed:", err);
  }
}

// ---- Auth (Fase 5) ---------------------------------------------------------
function initialsFrom(profile, email) {
  const name = profile?.username || `${profile?.first_name || ""}`.trim();
  if (name) return name.slice(0, 2).toUpperCase();
  if (email) return email[0].toUpperCase();
  return "?";
}

// Mostra a foto de perfil; se nao houver, mostra as iniciais (igual ao site).
function renderAvatar() {
  if (!els.accountAvatar) return;
  const p = state.profile;
  if (p?.avatar_url) {
    els.accountAvatar.style.backgroundImage = `url("${p.avatar_url}")`;
    els.accountAvatar.textContent = "";
  } else {
    els.accountAvatar.style.backgroundImage = "";
    els.accountAvatar.textContent = initialsFrom(p, state.session?.email);
  }
}

function setAccountUI() {
  if (state.session?.accessToken) {
    els.account.classList.remove("hidden");
    const p = state.profile;
    const display = p?.username || state.session.email || "signed in";
    els.accountEmail.textContent = display;
    renderAvatar();
  } else {
    els.account.classList.add("hidden");
  }
}

// Busca o perfil do produtor (nome + foto) e atualiza o topo. Via Rust
// (fetch_profile) porque a API do GPW nao envia CORS — fetch do webview seria
// bloqueado.
async function loadProfile() {
  try {
    const token = await getValidToken();
    const profile = await invoke("fetch_profile", { token });
    state.profile = profile || null;
    setAccountUI();
  } catch (err) {
    console.warn("loadProfile failed:", err);
  }
}

async function onLogin() {
  const email = els.loginEmail.value.trim();
  const password = els.loginPassword.value;
  if (!email || !password) {
    els.loginError.textContent = "Enter your email and password.";
    return;
  }
  els.loginBtn.disabled = true;
  els.loginBtn.textContent = "Signing in…";
  els.loginError.textContent = "";
  try {
    state.session = await login(email, password);
    setAccountUI();
    enterApp();
  } catch (err) {
    els.loginError.textContent = err.message || "Login failed.";
  } finally {
    els.loginBtn.disabled = false;
    els.loginBtn.textContent = "Log in";
  }
}

function onLogout() {
  clearSession();
  state.session = null;
  state.profile = null;
  setAccountUI();
  show(els.login);
}

// ---- Settings / defaults (Fase 5) ------------------------------------------
function fillSettings() {
  const d = state.defaults;
  els.defOs.value = d.os || "";
  els.defDaw.value = d.daw || "";
  els.defDawVersion.value = d.daw_version || "";
  els.defPlugins.value = d.plugins || "";
  els.defHardware.value = d.hardware || "";
  els.defPrice.value = d.default_price || 300;
  els.defGenre.value = d.default_genre || "";
  els.defProjectSale.checked = !!d.default_project_for_sale;
}

function onSettingsSave() {
  state.defaults = {
    ...state.defaults,
    os: els.defOs.value.trim(),
    daw: els.defDaw.value.trim(),
    daw_version: els.defDawVersion.value.trim(),
    plugins: els.defPlugins.value.trim(),
    hardware: els.defHardware.value.trim(),
    default_price: parseFloat(els.defPrice.value) || 300,
    default_genre: els.defGenre.value,
    default_project_for_sale: els.defProjectSale.checked,
  };
  saveDefaults(state.defaults);
  show(els.dropzone);
}

// ---- Last folder -----------------------------------------------------------
function setLastFolderUI() {
  if (state.lastFolder) {
    els.dropzoneLast.classList.remove("hidden");
    els.dropzoneLastFolder.textContent = state.lastFolder;
  } else {
    els.dropzoneLast.classList.add("hidden");
  }
}

function saveLastFolder(folder) {
  if (!folder) return;
  localStorage.setItem(LAST_FOLDER_KEY, folder);
  state.lastFolder = folder;
  setLastFolderUI();
}

// ---- Analysis (Fase 3) -----------------------------------------------------
async function analyzeMaster(master, run) {
  state.analysis = null;
  els.analysis.classList.remove("hidden");
  els.anBpm.textContent = "—";
  els.anKey.textContent = "—";
  els.anCamelot.textContent = "—";
  els.anStatus.textContent = "🎧 Analyzing the Extended Mix… detecting BPM & key…";
  els.anStatus.className = "analysis__status analysis__status--busy";

  if (typeof window.GPW_analyzeAudio !== "function") {
    els.anStatus.textContent = "⚠ Analysis engine not loaded.";
    els.anStatus.className = "analysis__status analysis__status--error";
    return;
  }

  try {
    const buf = await invoke("read_file_bytes", { path: master.path });
    const blob = new Blob([buf]);
    const r = await window.GPW_analyzeAudio(blob);
    if (run !== scanRun) return; // outra pasta foi escaneada entretanto
    state.analysis = r;
    els.anBpm.textContent = r.bpm || "—";
    els.anKey.textContent = r.keyName || "—";
    els.anCamelot.textContent = r.camelot || "—";
    els.anStatus.textContent = "✅ Detected from the Extended Mix — you can adjust on the site.";
    els.anStatus.className = "analysis__status analysis__status--ok";
  } catch (err) {
    if (run !== scanRun) return;
    els.anStatus.textContent = "⚠ Could not auto-detect — you can enter BPM & key on the site.";
    els.anStatus.className = "analysis__status analysis__status--error";
    console.error("GPW_analyzeAudio falhou:", err);
  }
}

// ---- Conversion (Fase 4) ---------------------------------------------------
function buildMasters(scan) {
  const existing = new Set(scan.files.map((f) => f.upload_field).filter(Boolean));
  const out = [];
  for (const f of scan.files) {
    const m = MP3_MAP[f.category];
    if (m && !existing.has(m.field)) {
      out.push({ path: f.path, field: m.field, out_name: m.out_name, label: m.label });
    }
  }
  return out;
}

function renderConvRows(masters) {
  els.conversionList.innerHTML = "";
  state.convRows = {};
  for (const m of masters) {
    const li = el("li", "conv-row");
    const main = el("div", "conv-row__main");
    main.appendChild(el("span", "conv-row__label", m.label));
    main.appendChild(el("span", "conv-row__name", m.out_name));
    const status = el("span", "conv-row__status", "Queued…");
    li.appendChild(main);
    li.appendChild(status);
    els.conversionList.appendChild(li);
    state.convRows[m.field] = { li, status };
  }
}

function onConvProgress(p) {
  const row = state.convRows[p.field];
  if (!row) return;
  if (p.status === "start") {
    row.status.textContent = "Converting…";
    row.status.className = "conv-row__status conv-row__status--busy";
  } else if (p.status === "done") {
    row.status.textContent = "Done";
    row.status.className = "conv-row__status conv-row__status--ok";
  } else if (p.status === "error") {
    row.status.textContent = "Failed";
    row.status.className = "conv-row__status conv-row__status--error";
    if (p.error) row.li.title = p.error;
  }
}

function warnLine(msg) {
  els.resultsWarnings.appendChild(el("div", "warn-line", msg));
}

async function convertMasters(scan, run) {
  const masters = buildMasters(scan);
  state.converted = [];
  if (!masters.length) {
    els.conversion.classList.add("hidden");
    // Sem conversao: libera o "Continue" se houver master.
    els.continueBtn.disabled = !scan.has_extended_master;
    return;
  }
  els.conversion.classList.remove("hidden");
  renderConvRows(masters);
  // Bloqueia o "Continue" ate a conversao terminar — senao os MP3s nao entram.
  els.continueBtn.disabled = true;
  els.continueStatus.textContent = "Converting MP3s…";
  try {
    const results = await invoke("convert_masters", { masters });
    if (run !== scanRun) return; // outra pasta foi escaneada entretanto
    state.converted = results;
    for (const r of results) {
      const row = state.convRows[r.field];
      if (row && r.ok) row.status.textContent = `Done · ${formatSize(r.size)}`;
    }
    const failed = results.filter((r) => !r.ok);
    if (failed.length) {
      warnLine(`⚠ MP3 conversion failed for ${failed.map((r) => r.label).join(", ")} — the upload will continue without them.`);
    }
  } catch (err) {
    if (run !== scanRun) return;
    console.error("convert_masters falhou:", err);
    warnLine("⚠ MP3 conversion failed — the upload will continue without MP3s.");
  } finally {
    if (run === scanRun) {
      els.continueStatus.textContent = "";
      els.continueBtn.disabled = !scan.has_extended_master;
    }
  }
}

// ---- Scan (Fase 2) ---------------------------------------------------------
async function handleDrop(paths) {
  if (!paths || paths.length === 0) return;
  if (state.uploading) return; // upload em curso: nao troca de track no meio
  const folder = paths[0];
  const run = ++scanRun; // invalida analise/conversao da rodada anterior

  state.scan = null;
  state.analysis = null;
  state.draftId = null;
  state.uploadedFields = new Set();
  els.continueBtn.disabled = true;
  els.continueStatus.textContent = "";
  els.continueResult.classList.add("hidden");
  els.continueResult.innerHTML = "";
  els.uploadProgress.classList.add("hidden");
  els.uploadProgress.innerHTML = "";
  state.upRows = {};
  els.resultsPath.textContent = folder;
  els.resultsSummary.innerHTML = '<div class="loading">Scanning…</div>';
  els.resultsWarnings.innerHTML = "";
  els.resultsList.innerHTML = "";
  els.analysis.classList.add("hidden");
  els.conversion.classList.add("hidden");
  show(els.results);

  try {
    const result = await invoke("scan_folder", { folder });
    if (run !== scanRun) return;
    state.scan = result;
    renderScan(result, els);
    saveLastFolder(result.folder); // pode ser a pasta pai, se um arquivo foi arrastado

    // O "Continue" fica desabilitado ate a conversao MP3 terminar
    // (convertMasters libera no fim) — garante que os MP3s entrem no upload.
    const master = result.files.find((f) => f.category === "extended_mix");
    if (master) analyzeMaster(master, run);
    convertMasters(result, run);
    runQc(result, () => run === scanRun, els.resultsWarnings).catch((e) =>
      console.error("QC falhou:", e)
    );
  } catch (err) {
    if (run !== scanRun) return;
    els.resultsSummary.innerHTML = "";
    els.resultsWarnings.innerHTML = "";
    els.resultsWarnings.appendChild(el("div", "warn-line warn-line--error", `Scan failed: ${err}`));
    console.error("scan_folder falhou:", err);
  }
}

// ---- Continue: cria rascunho no site e abre upload.html (Fase nova) ---------
// Monta a lista de arquivos para o multipart (master `file`, cover, xf_*).
// Um arquivo por campo: se dois arquivos caem no mesmo slot (duas capas, dois
// WAVs com "mix" no nome), o primeiro na ordem do scan vence — o scanner-ui
// avisa quais ficaram de fora. Sem isso o multipart ia com o campo duplicado
// e as barras de progresso (chaveadas por campo) colidiam.
function buildUploadFiles() {
  const files = [];
  const seen = new Set();
  const push = (field, path, filename) => {
    if (seen.has(field)) return;
    seen.add(field);
    files.push({ field, path, filename });
  };
  for (const f of state.scan.files) {
    if (f.upload_field) push(f.upload_field, f.path, f.filename);
  }
  for (const c of state.converted) {
    if (c.ok) push(c.field, c.path, c.out_name);
  }
  return files;
}

// Desenha uma barra de progresso por arquivo (chaveada pelo campo do multipart).
function renderUploadRows(files) {
  els.uploadProgress.innerHTML = "";
  state.upRows = {};
  for (const f of files) {
    const row = el("div", "up-row");
    const top = el("div", "up-row__top");
    top.appendChild(el("span", "up-row__name", f.filename));
    const pct = el("span", "up-row__pct", "0%");
    top.appendChild(pct);
    const bar = el("div", "up-bar");
    const fill = el("div", "up-bar__fill");
    bar.appendChild(fill);
    row.appendChild(top);
    row.appendChild(bar);
    els.uploadProgress.appendChild(row);
    state.upRows[f.field] = { row, fill, pct };
  }
  els.uploadProgress.classList.remove("hidden");
}

// Atualiza a barra conforme os eventos "upload:file-progress" do backend.
function onFileProgress(p) {
  const r = state.upRows[p.field];
  if (!r) return;
  r.fill.style.width = p.percent + "%";
  r.pct.textContent = p.percent + "%";
  if (p.percent >= 100) r.row.classList.add("up-row--done");
}

function showCancel(show) {
  els.cancelBtn.classList.toggle("hidden", !show);
  els.cancelBtn.disabled = false;
  els.cancelBtn.textContent = "Cancel";
}

// Pede o cancelamento: o loop de arquivos para no proximo, e o Rust aborta o
// stream do arquivo em transito. O rascunho fica no site — Retry retoma.
function onCancelUpload() {
  state.cancelRequested = true;
  els.cancelBtn.disabled = true;
  els.cancelBtn.textContent = "Cancelling…";
  invoke("set_upload_cancelled", { cancelled: true }).catch(() => {});
}

function continueError(msg) {
  els.continueResult.classList.remove("hidden");
  els.continueResult.className = "publish-result publish-result--error";
  els.continueResult.innerHTML = "";
  els.continueResult.appendChild(el("h3", "publish-result__title", "⚠ Could not continue"));
  els.continueResult.appendChild(el("p", "publish-result__msg", msg));
  const retry = el("button", "btn btn--primary btn--sm", "Retry");
  retry.addEventListener("click", onContinue);
  els.continueResult.appendChild(retry);
}

async function onContinue() {
  if (!state.scan || state.uploading) return; // evita upload duplo (clique duplo)
  const d = state.defaults;
  const a = state.analysis;

  const files = buildUploadFiles();
  if (!files.some((f) => f.field === "file")) {
    continueError("Missing the Extended Mix master file.");
    return;
  }

  // Metadata que o app ja conhece (o resto o produtor preenche no site).
  // "Project file for sale" e a preferencia do produtor (Settings); pre-preenche
  // o chkProject do site, que ele pode ajustar por track.
  const hasProject = !!d.default_project_for_sale;
  const metadata = {
    os: d.os,
    daw: d.daw,
    daw_version: d.daw_version,
    plugins: d.plugins,
    hardware: d.hardware,
  };
  // Genero padrao (opcional): pre-seleciona o estilo no site.
  if (d.default_genre) metadata.genres = [d.default_genre];
  const fields = [
    ["genre", d.default_genre || ""],
    ["bpm", a?.bpm ? String(a.bpm) : ""],
    ["music_key", a?.keyName || ""],
    ["daw", `${d.daw} ${d.daw_version}`.trim()],
    ["price_eur", String(d.default_price || 200)],
    ["has_project_file", String(hasProject)],
    ["metadata", JSON.stringify(metadata)],
  ];

  try {
    await getValidToken();
  } catch (err) {
    continueError(err.message || "You need to log in again.");
    return;
  }

  state.uploading = true;
  state.cancelRequested = false;
  invoke("set_upload_cancelled", { cancelled: false }).catch(() => {});
  els.continueBtn.disabled = true;
  els.continueResult.classList.add("hidden");
  els.continueStatus.textContent = "Uploading files…";
  renderUploadRows(files);
  showCancel(true);

  // Sobe UM arquivo por requisicao: master + capa criam o rascunho; os demais
  // (xf_*) vao um a um — evita 502 por memoria/timeout em uploads grandes.
  const base = files.filter((f) => f.field === "file" || f.field === "cover");
  const extras = files.filter((f) => f.field.startsWith("xf_"));

  try {
    let token = await getValidToken();

    // Retry depois de falha parcial: reusa o rascunho ja criado em vez de
    // criar outro (evita rascunhos duplicados no site).
    let draftId = state.draftId;
    if (!draftId) {
      const res = await invoke("create_draft", { payload: { token, fields, files: base } });
      if (!res.ok || !res.id) {
        els.continueStatus.textContent = "";
        els.continueBtn.disabled = false;
        continueError(res.message || "Could not create the draft.");
        return;
      }
      draftId = res.id;
      state.draftId = draftId;
      for (const f of base) state.uploadedFields.add(f.field);
    }
    // Marca como concluidas as barras do que ja subiu (base ou retry).
    for (const field of state.uploadedFields) onFileProgress({ field, percent: 100 });

    const failed = [];
    for (const f of extras) {
      if (state.cancelRequested) break;
      if (state.uploadedFields.has(f.field)) continue; // ja subiu num retry anterior
      els.continueStatus.textContent = `Uploading ${f.filename}…`;
      try {
        token = await getValidToken(); // renova se necessario entre arquivos
        const r = await invoke("add_draft_file", { token, draftId, file: f });
        if (r.ok) state.uploadedFields.add(f.field);
        else failed.push(`${f.filename}: ${r.message}`);
      } catch (err) {
        if (state.cancelRequested) break;
        failed.push(`${f.filename}: ${err}`);
      }
    }

    if (state.cancelRequested) {
      els.continueStatus.textContent = "";
      els.continueBtn.disabled = false;
      continueError("Upload cancelled — Retry resumes from where it stopped.");
      return;
    }

    // Abre a pagina de upload do site com auto-login (token no hash) + draft id.
    token = await getValidToken();
    const hash = `#gpw_at=${encodeURIComponent(token)}&gpw_rt=${encodeURIComponent(currentSession()?.refreshToken || "")}`;
    const url = `${GPW_BASE}/upload.html?edit=${encodeURIComponent(draftId)}${hash}`;
    let opened = true;
    try { await openExternal(url); } catch (e) { opened = false; console.warn("open failed:", e); }
    renderContinueSuccess({ warnings: failed }, url, opened);
  } catch (err) {
    els.continueStatus.textContent = "";
    els.continueBtn.disabled = false;
    continueError(String(err));
  } finally {
    state.uploading = false;
    showCancel(false);
  }
}

function renderContinueSuccess(res, url, opened) {
  els.continueStatus.textContent = "";
  // Mantem o botao desabilitado: ja foi enviado, evita criar rascunho duplicado.
  // Para outra track, arraste uma nova pasta. Para reabrir, use o botao abaixo.
  els.continueBtn.disabled = true;
  els.continueResult.classList.remove("hidden");
  els.continueResult.className = "publish-result publish-result--ok";
  els.continueResult.innerHTML = "";
  els.continueResult.appendChild(el("h3", "publish-result__title", "✅ Files uploaded — finish on the site"));
  els.continueResult.appendChild(
    el(
      "p",
      "publish-result__msg",
      opened
        ? "The upload page opened in your browser with the files already attached. Fill in the remaining details there and submit for review."
        : "We couldn't open your browser automatically — use the button below to open the upload page and finish there."
    )
  );
  const open = el("button", "btn btn--ghost btn--sm", opened ? "Open upload page again" : "Open upload page");
  open.addEventListener("click", () => openExternal(url).catch((e) => console.warn("open failed:", e)));
  els.continueResult.appendChild(open);

  for (const w of res.warnings || []) {
    els.continueResult.appendChild(el("div", "warn-line", w));
  }
}

function onUploadProgress(p) {
  els.continueStatus.textContent = p.message || "";
}

// ---- Boot ------------------------------------------------------------------
function enterApp() {
  fillSettings();
  setLastFolderUI();
  show(els.dropzone);
  loadProfile(); // busca foto/nome em segundo plano
}

// Abre o seletor nativo de pasta (alternativa ao arrastar).
async function onBrowse() {
  try {
    const dialog = window.__TAURI__.dialog;
    if (!dialog?.open) return;
    const folder = await dialog.open({
      directory: true,
      multiple: false,
      title: "Select your export folder",
    });
    if (folder) handleDrop([folder]);
  } catch (err) {
    console.error("browse failed:", err);
  }
}

async function wireDragDrop() {
  const webview = getCurrentWebview();
  await webview.onDragDropEvent((event) => {
    const { type } = event.payload;
    if (type === "enter" || type === "over") {
      els.dropzone.classList.add("dropzone--over");
    } else if (type === "leave") {
      els.dropzone.classList.remove("dropzone--over");
    } else if (type === "drop") {
      els.dropzone.classList.remove("dropzone--over");
      if (!els.dropzone.classList.contains("hidden")) handleDrop(event.payload.paths);
    }
  });
}

window.addEventListener("DOMContentLoaded", async () => {
  cacheEls();
  state.lastFolder = localStorage.getItem(LAST_FOLDER_KEY) || "";

  // Auth (form: Enter submete a partir de qualquer campo)
  els.loginForm.addEventListener("submit", (e) => { e.preventDefault(); onLogin(); });
  els.logoutBtn.addEventListener("click", onLogout);
  // Settings
  els.settingsBtn.addEventListener("click", () => { fillSettings(); show(els.settings); });
  els.settingsClose.addEventListener("click", () => show(els.dropzone));
  els.settingsSave.addEventListener("click", onSettingsSave);
  // Results
  els.resetBtn.addEventListener("click", () => show(els.dropzone));
  els.rescanBtn.addEventListener("click", () => { if (state.lastFolder) handleDrop([state.lastFolder]); });
  els.browseBtn.addEventListener("click", onBrowse);
  els.continueBtn.addEventListener("click", onContinue);
  els.cancelBtn.addEventListener("click", onCancelUpload);

  // Auto-update
  els.updateBtn.addEventListener("click", doUpdate);
  els.updateDismiss.addEventListener("click", () => els.updateBar.classList.add("hidden"));

  healthcheck();
  checkForUpdates();
  wireDragDrop();
  listen("convert:progress", (e) => onConvProgress(e.payload));
  listen("upload:progress", (e) => onUploadProgress(e.payload));
  listen("upload:file-progress", (e) => onFileProgress(e.payload));

  // Sessao persistida -> renova o token (fica sempre logado) e entra direto.
  state.session = await loadSession();
  setAccountUI();
  if (state.session?.accessToken) {
    try {
      await getValidToken();          // renova se estiver perto de expirar
      state.session = currentSession();
      setAccountUI();
      enterApp();
    } catch (err) {
      console.warn("session refresh failed:", err);
      show(els.login);
    }
  } else {
    show(els.login);
  }
});
