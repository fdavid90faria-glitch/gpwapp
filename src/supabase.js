// supabase.js (Fase 5) - login do produtor + sessao persistida.
//
// A sessao e gravada em arquivo (comandos Rust save_auth/load_auth/clear_auth),
// que sobrevive a fechar/reabrir o app (o localStorage do webview nao e
// confiavel entre reaberturas). No boot, o app renova o token pelo refresh
// token -> o produtor fica sempre logado ate deslogar manualmente.

import { SUPABASE_URL, SUPABASE_ANON_KEY } from "./config.js";

const { invoke } = window.__TAURI__.core;

let _session = null; // cache em memoria da sessao atual

function persist(session) {
  _session = session;
  invoke("save_auth", { data: JSON.stringify(session) }).catch((e) =>
    console.warn("save_auth failed:", e)
  );
}

/// Sessao atual em memoria (sincrono). Use depois de loadSession()/login().
export function currentSession() {
  return _session;
}

/// Carrega a sessao do disco para a memoria (chamado no boot). Async.
export async function loadSession() {
  try {
    const raw = await invoke("load_auth");
    _session = raw ? JSON.parse(raw) : null;
  } catch (err) {
    console.warn("load_auth failed:", err);
    _session = null;
  }
  return _session;
}

export function clearSession() {
  _session = null;
  invoke("clear_auth").catch((e) => console.warn("clear_auth failed:", e));
}

function toSession(data) {
  return {
    accessToken: data.access_token,
    refreshToken: data.refresh_token,
    expiresAt: data.expires_at || 0, // epoch seconds
    email: data.user?.email || _session?.email || "",
  };
}

async function tokenRequest(grant, body) {
  const res = await fetch(`${SUPABASE_URL}/auth/v1/token?grant_type=${grant}`, {
    method: "POST",
    headers: {
      apikey: SUPABASE_ANON_KEY,
      "Content-Type": "application/json",
    },
    body: JSON.stringify(body),
  });
  const data = await res.json().catch(() => ({}));
  if (!res.ok) {
    const msg =
      data.error_description || data.msg || data.error || "Login failed.";
    throw new Error(msg);
  }
  return toSession(data);
}

/// Faz login com email/senha e persiste a sessao.
export async function login(email, password) {
  const session = await tokenRequest("password", { email, password });
  persist(session);
  return session;
}

/// Renova a sessao a partir do refresh_token e persiste.
async function refresh(refreshToken) {
  const session = await tokenRequest("refresh_token", {
    refresh_token: refreshToken,
  });
  persist(session);
  return session;
}

/// Devolve um access_token valido (renova se faltar < 60s para expirar, ou se
/// nao soubermos quando expira). Lanca se nao houver sessao ou a renovacao
/// falhar. Chamadas concorrentes compartilham a mesma renovacao — o refresh
/// token do Supabase e de uso unico, dois refreshes paralelos invalidariam a
/// sessao.
let _refreshing = null;

export async function getValidToken() {
  if (!_session?.accessToken) throw new Error("Not logged in.");
  const now = Math.floor(Date.now() / 1000);
  const needsRefresh = !_session.expiresAt || _session.expiresAt - now < 60;
  if (needsRefresh) {
    if (!_session.refreshToken) throw new Error("Session expired. Log in again.");
    if (!_refreshing) {
      _refreshing = refresh(_session.refreshToken).finally(() => {
        _refreshing = null;
      });
    }
    const fresh = await _refreshing;
    return fresh.accessToken;
  }
  return _session.accessToken;
}
