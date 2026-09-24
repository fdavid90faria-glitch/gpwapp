// supabase.js (Fase 5) - login do produtor + sessao persistida.
//
// A sessao e gravada em arquivo (comandos Rust save_auth/load_auth/clear_auth),
// que sobrevive a fechar/reabrir o app (o localStorage do webview nao e
// confiavel entre reaberturas). No boot, o app renova o token pelo refresh
// token -> o produtor fica sempre logado ate deslogar manualmente.

import { SUPABASE_URL, SUPABASE_ANON_KEY } from "./config.js";

const { invoke } = window.__TAURI__.core;

let _session = null; // cache em memoria da sessao atual

// Grava a sessao e SO devolve depois de estar em disco. O Supabase rotaciona o
// refresh token a cada renovacao (o antigo deixa de servir); se o app fechasse
// antes desta escrita, ficava em disco um token ja invalido -> deslogava no
// arranque seguinte. Por isso esperamos pela escrita.
async function persist(session) {
  _session = session;
  try {
    await invoke("save_auth", { data: JSON.stringify(session) });
  } catch (e) {
    console.warn("save_auth failed:", e);
  }
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
    // epoch seconds; o verify do 2FA devolve so expires_in
    expiresAt: data.expires_at || (data.expires_in ? Math.floor(Date.now() / 1000) + data.expires_in : 0),
    email: data.user?.email || _session?.email || "",
  };
}

// Pedido autenticado a /auth/v1 (usado no 2FA, com o token da 1.a etapa).
async function authPost(path, accessToken, body) {
  const res = await fetch(`${SUPABASE_URL}/auth/v1${path}`, {
    method: "POST",
    headers: {
      apikey: SUPABASE_ANON_KEY,
      Authorization: `Bearer ${accessToken}`,
      "Content-Type": "application/json",
    },
    body: JSON.stringify(body || {}),
  });
  const data = await res.json().catch(() => ({}));
  if (!res.ok) throw new Error(data.error_description || data.msg || data.message || "Verification failed.");
  return data;
}

async function tokenRequest(grant, body, { raw = false } = {}) {
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
  return raw ? data : toSession(data);
}

/// Faz login com email/senha e persiste a sessao.
/// Conta com 2FA: NAO persiste nada e devolve { mfa: { factorId, token } } — o
/// site so aceita a sessao depois do codigo (aal2), por isso o app pede-o e
/// chama verifyMfa().
export async function login(email, password) {
  const data = await tokenRequest("password", { email, password }, { raw: true });
  const factor = (data.user?.factors || []).find((f) => f.status === "verified" && f.factor_type === "totp");
  if (factor) return { mfa: { factorId: factor.id, token: data.access_token, email: data.user?.email || email } };
  const session = toSession(data);
  await persist(session);
  return session;
}

/// 2.a etapa do login com 2FA: codigo de 6 digitos da app de autenticacao.
export async function verifyMfa(mfa, code) {
  const ch = await authPost(`/factors/${mfa.factorId}/challenge`, mfa.token, {});
  const data = await authPost(`/factors/${mfa.factorId}/verify`, mfa.token, { challenge_id: ch.id, code });
  const session = toSession({ ...data, user: data.user || { email: mfa.email } });
  await persist(session);
  return session;
}

/// Renova a sessao a partir do refresh_token e persiste.
async function refresh(refreshToken) {
  const session = await tokenRequest("refresh_token", {
    refresh_token: refreshToken,
  });
  await persist(session);
  return session;
}

/// Devolve um access_token valido (renova se faltar < 60s para expirar, ou se
/// nao soubermos quando expira). Lanca se nao houver sessao ou a renovacao
/// falhar. Chamadas concorrentes compartilham a mesma renovacao — o refresh
/// token do Supabase e de uso unico, dois refreshes paralelos invalidariam a
/// sessao.
let _refreshing = null;

export async function getValidToken({ minRemaining = 60 } = {}) {
  if (!_session?.accessToken) throw new Error("Not logged in.");
  const now = Math.floor(Date.now() / 1000);
  // minRemaining: quem vai usar o token numa operacao longa (upload) pede uma
  // margem maior para nao apanhar um token que expira a meio.
  const needsRefresh = !_session.expiresAt || _session.expiresAt - now < minRemaining;
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
