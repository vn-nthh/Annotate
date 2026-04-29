/**
 * Google Drive Sync for Annotate.
 *
 * Handles OAuth, token management, and auto-syncing dictionary & history
 * to Google Drive appDataFolder (hidden, app-specific storage).
 *
 * - Dictionary: tombstone merge (additions + deletions propagate across devices)
 * - History: merge strategy (union deduped by text, newest timestamp wins)
 */

const { invoke } = window.__TAURI__.core;
const { load } = window.__TAURI__.store;

// ── Config ─────────────────────────────────────────────
const CLIENT_ID = '714943573390-rjel4u4pd0ns6clf36993a08i4djqhqi.apps.googleusercontent.com';
const CLIENT_SECRET = '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';
const SCOPES = 'https://www.googleapis.com/auth/drive.appdata https://www.googleapis.com/auth/userinfo.profile https://www.googleapis.com/auth/userinfo.email';

const DRIVE_API = 'https://www.googleapis.com/drive/v3';
const UPLOAD_API = 'https://www.googleapis.com/upload/drive/v3';

// File names in appDataFolder
const DICT_FILE = 'annotate_dictionary.json';
const HISTORY_FILE = 'annotate_history.json';

// ── State ──────────────────────────────────────────────
let store = null;
let accessToken = null;
let refreshToken = null;
let tokenExpiry = 0;
let userInfo = null;
let syncInProgress = false;
let syncTimer = null;

// ── Callbacks for UI updates ───────────────────────────
let onSyncStatusChange = null;
let onSignInChange = null;

export function setSyncCallbacks({ onStatus, onSignIn }) {
  onSyncStatusChange = onStatus;
  onSignInChange = onSignIn;
}

function emitStatus(status, detail = '') {
  if (onSyncStatusChange) onSyncStatusChange(status, detail);
}

function emitSignIn(signedIn, user) {
  if (onSignInChange) onSignInChange(signedIn, user);
}

// ── Init ───────────────────────────────────────────────
export async function initSync() {
  store = await load('settings.json', { autoSave: true });

  // Restore saved tokens
  accessToken = await store.get('gd_access_token');
  refreshToken = await store.get('gd_refresh_token');
  tokenExpiry = (await store.get('gd_token_expiry')) || 0;
  userInfo = await store.get('gd_user_info');

  if (refreshToken) {
    emitSignIn(true, userInfo);
    // Try to refresh and sync
    try {
      await ensureValidToken();
      emitStatus('synced', 'Connected');
      startAutoSync();
    } catch (err) {
      console.warn('[Sync] Token refresh failed, need re-auth:', err);
      emitStatus('error', 'Session expired');
    }
  } else {
    emitSignIn(false, null);
    emitStatus('disconnected', 'Not signed in');
  }
}

// ── Auth ───────────────────────────────────────────────
export async function signIn() {
  emitStatus('syncing', 'Signing in\u2026');

  try {
    // 1. Tauri loopback OAuth
    const result = await invoke('google_oauth', {
      clientId: CLIENT_ID,
      scopes: SCOPES,
    });

    // 2. Exchange auth code for tokens
    const tokenResp = await fetch('https://oauth2.googleapis.com/token', {
      method: 'POST',
      headers: { 'Content-Type': 'application/x-www-form-urlencoded' },
      body: new URLSearchParams({
        code: result.code,
        client_id: CLIENT_ID,
        client_secret: CLIENT_SECRET,
        redirect_uri: result.redirect_uri,
        grant_type: 'authorization_code',
      }),
    });

    if (!tokenResp.ok) {
      const err = await tokenResp.json().catch(() => ({}));
      throw new Error(err.error_description || err.error || `HTTP ${tokenResp.status}`);
    }

    const tokens = await tokenResp.json();
    accessToken = tokens.access_token;
    refreshToken = tokens.refresh_token || refreshToken;
    tokenExpiry = Date.now() + (tokens.expires_in - 60) * 1000;

    // Save tokens
    await store.set('gd_access_token', accessToken);
    await store.set('gd_refresh_token', refreshToken);
    await store.set('gd_token_expiry', tokenExpiry);

    // 3. Fetch user profile
    const profileResp = await fetch('https://www.googleapis.com/oauth2/v2/userinfo', {
      headers: { Authorization: `Bearer ${accessToken}` },
    });
    userInfo = await profileResp.json();
    await store.set('gd_user_info', userInfo);

    emitSignIn(true, userInfo);
    emitStatus('synced', 'Signed in');

    // 4. Initial sync
    await syncNow();
    startAutoSync();

    return userInfo;
  } catch (err) {
    console.error('[Sync] Sign in failed:', err);
    emitStatus('error', 'Sign in failed');
    throw err;
  }
}

export async function signOut() {
  accessToken = null;
  refreshToken = null;
  tokenExpiry = 0;
  userInfo = null;

  await store.delete('gd_access_token');
  await store.delete('gd_refresh_token');
  await store.delete('gd_token_expiry');
  await store.delete('gd_user_info');

  stopAutoSync();
  emitSignIn(false, null);
  emitStatus('disconnected', 'Signed out');
}

export function isSignedIn() {
  return !!refreshToken;
}

export function getUser() {
  return userInfo;
}

// ── Token Management ───────────────────────────────────
async function ensureValidToken() {
  if (accessToken && Date.now() < tokenExpiry) return;
  if (!refreshToken) throw new Error('No refresh token');

  const resp = await fetch('https://oauth2.googleapis.com/token', {
    method: 'POST',
    headers: { 'Content-Type': 'application/x-www-form-urlencoded' },
    body: new URLSearchParams({
      client_id: CLIENT_ID,
      client_secret: CLIENT_SECRET,
      refresh_token: refreshToken,
      grant_type: 'refresh_token',
    }),
  });

  if (!resp.ok) {
    const err = await resp.json().catch(() => ({}));
    // If refresh token is revoked, clear auth
    if (resp.status === 400 || resp.status === 401) {
      await signOut();
    }
    throw new Error(err.error_description || err.error || `Refresh failed: ${resp.status}`);
  }

  const tokens = await resp.json();
  accessToken = tokens.access_token;
  tokenExpiry = Date.now() + (tokens.expires_in - 60) * 1000;

  await store.set('gd_access_token', accessToken);
  await store.set('gd_token_expiry', tokenExpiry);
}

// ── Google Drive Helpers ───────────────────────────────
async function driveGet(path, params = {}) {
  await ensureValidToken();
  const url = new URL(`${DRIVE_API}${path}`);
  Object.entries(params).forEach(([k, v]) => url.searchParams.set(k, v));

  const resp = await fetch(url, {
    headers: { Authorization: `Bearer ${accessToken}` },
  });

  if (!resp.ok) throw new Error(`Drive GET ${path}: ${resp.status}`);
  return resp.json();
}

async function findFile(name) {
  const data = await driveGet('/files', {
    spaces: 'appDataFolder',
    q: `name='${name}' and trashed=false`,
    fields: 'files(id,name,modifiedTime)',
    pageSize: '1',
  });
  return data.files?.[0] || null;
}

async function readFile(fileId) {
  await ensureValidToken();
  const resp = await fetch(`${DRIVE_API}/files/${fileId}?alt=media`, {
    headers: { Authorization: `Bearer ${accessToken}` },
  });
  if (!resp.ok) throw new Error(`Drive read: ${resp.status}`);
  return resp.json();
}

async function writeFile(name, data, existingFileId = null) {
  await ensureValidToken();
  const content = JSON.stringify(data);

  if (existingFileId) {
    // Update existing file
    const resp = await fetch(`${UPLOAD_API}/files/${existingFileId}?uploadType=media`, {
      method: 'PATCH',
      headers: {
        Authorization: `Bearer ${accessToken}`,
        'Content-Type': 'application/json',
      },
      body: content,
    });
    if (!resp.ok) throw new Error(`Drive update: ${resp.status}`);
    return resp.json();
  } else {
    // Create new file in appDataFolder
    // Use multipart upload to set metadata + content
    const metadata = {
      name,
      parents: ['appDataFolder'],
    };

    const boundary = 'annotate_boundary_' + Date.now();
    const body =
      `--${boundary}\r\n` +
      `Content-Type: application/json; charset=UTF-8\r\n\r\n` +
      `${JSON.stringify(metadata)}\r\n` +
      `--${boundary}\r\n` +
      `Content-Type: application/json\r\n\r\n` +
      `${content}\r\n` +
      `--${boundary}--`;

    const resp = await fetch(`${UPLOAD_API}/files?uploadType=multipart`, {
      method: 'POST',
      headers: {
        Authorization: `Bearer ${accessToken}`,
        'Content-Type': `multipart/related; boundary=${boundary}`,
      },
      body,
    });
    if (!resp.ok) throw new Error(`Drive create: ${resp.status}`);
    return resp.json();
  }
}

// ── Sync Logic ─────────────────────────────────────────

/**
 * Dictionary sync — TOMBSTONE MERGE strategy.
 *
 * Format: { terms: string[], deleted: string[] }
 *
 * Merge rules (per term):
 *   - Local tombstone beats a stale remote presence (delete wins)
 *   - Remote tombstone beats a stale local presence (delete wins)
 *   - A term is only resurrected if it is explicitly re-added AFTER deletion
 *     (which lifts the tombstone via addDictTerm in main.js)
 *
 * This ensures deletions always propagate correctly on the first sync
 * after the delete, even before the remote has been updated.
 */
async function syncDictionary() {
  const local = getLocalDictionary();

  const file = await findFile(DICT_FILE);

  if (file) {
    const remoteRaw = await readFile(file.id);
    const remote = normalizeDictStore(remoteRaw);

    // Merge with tombstones
    const merged = mergeDictionaries(local, remote);

    // Save merged locally
    saveLocalDictionary(merged);

    // Push merged to Drive
    await writeFile(DICT_FILE, merged, file.id);

    console.log(`[Sync] Dictionary merged: ${local.terms.length} local + ${remote.terms.length} remote -> ${merged.terms.length} terms (${merged.deleted.length} tombstones)`);
    return merged.terms;
  } else {
    // No remote — push local
    if (local.terms.length > 0 || local.deleted.length > 0) {
      await writeFile(DICT_FILE, local);
      console.log(`[Sync] Dictionary uploaded: ${local.terms.length} terms`);
    }
    return local.terms;
  }
}

/**
 * Normalize remote data to { terms, deleted } format.
 * Handles both the old string[] format and the new tombstone format.
 */
function normalizeDictStore(raw) {
  if (Array.isArray(raw)) {
    return { terms: raw, deleted: [] };
  }
  return {
    terms: Array.isArray(raw?.terms) ? raw.terms : [],
    deleted: Array.isArray(raw?.deleted) ? raw.deleted : [],
  };
}

/**
 * Merge two dictionary stores with tombstone awareness.
 *
 * Survival rule per term:
 *   - A term survives only if it is alive on a side that has NOT
 *     explicitly tombstoned it on the other side.
 *
 *   isAliveLocal  = in local.terms  && NOT in local.deleted
 *   isAliveRemote = in remote.terms && NOT in remote.deleted
 *
 *   Survives if:
 *     isAliveLocal  && !localDeletedSet (trivially true — local itself is alive)
 *     OR
 *     isAliveRemote && NOT locally tombstoned   ← key fix: local delete wins
 *
 *   Simplified to:
 *     isAliveLocal || (isAliveRemote && !localDeletedSet.has(key))
 *
 *   Symmetrically, a remote tombstone also beats a stale local presence:
 *     isAliveLocal && !remoteDeletedSet.has(key)  || isAliveRemote
 *
 *   Combined (either explicit delete wins):
 *     (isAliveLocal  && !remoteDeletedSet.has(key))
 *     || (isAliveRemote && !localDeletedSet.has(key))
 *
 * Re-adding a term always lifts its local tombstone first (in addDictTerm),
 * so re-adds still work correctly.
 */
function mergeDictionaries(local, remote) {
  // Build lookup sets for each side
  const localTermSet = new Set(local.terms.map(t => t.toLowerCase().trim()));
  const localDeletedSet = new Set(local.deleted.map(t => t.toLowerCase().trim()));
  const remoteTermSet = new Set(remote.terms.map(t => t.toLowerCase().trim()));
  const remoteDeletedSet = new Set(remote.deleted.map(t => t.toLowerCase().trim()));

  // A term is "alive" on a side if present in terms but NOT in its own deleted list
  const isAliveLocal  = (key) => localTermSet.has(key)  && !localDeletedSet.has(key);
  const isAliveRemote = (key) => remoteTermSet.has(key) && !remoteDeletedSet.has(key);

  // Collect all unique terms and tombstones (preserving display casing)
  const allTerms = new Map();
  for (const t of [...local.terms, ...remote.terms]) {
    const key = t.toLowerCase().trim();
    if (!allTerms.has(key)) allTerms.set(key, t.trim());
  }

  const allDeleted = new Map();
  for (const t of [...local.deleted, ...remote.deleted]) {
    const key = t.toLowerCase().trim();
    if (!allDeleted.has(key)) allDeleted.set(key, t.trim());
  }

  // Resolve survival:
  // A term survives only if it is alive on a side AND the other side
  // has NOT explicitly tombstoned it. An explicit tombstone always wins
  // over a stale (not-yet-synced) presence on the other side.
  const finalTerms = [];
  for (const [key, display] of allTerms) {
    const localAliveAndNotRemoteDeleted  = isAliveLocal(key)  && !remoteDeletedSet.has(key);
    const remoteAliveAndNotLocalDeleted  = isAliveRemote(key) && !localDeletedSet.has(key);
    if (localAliveAndNotRemoteDeleted || remoteAliveAndNotLocalDeleted) {
      finalTerms.push(display);
    }
  }

  // Keep a tombstone if the term did not survive (i.e. was not re-added on
  // either side after the deletion).
  const survivingSet = new Set(finalTerms.map(t => t.toLowerCase().trim()));
  const finalDeleted = [];
  for (const [key, display] of allDeleted) {
    if (!survivingSet.has(key)) {
      finalDeleted.push(display);
    }
  }

  finalTerms.sort((a, b) => a.toLowerCase().localeCompare(b.toLowerCase()));

  return { terms: finalTerms, deleted: finalDeleted };
}

/**
 * History sync — MERGE strategy.
 * Union of local and remote entries, deduped by text, newest-first, capped at 50.
 */
async function syncHistory() {
  const localHistory = getLocalHistory();
  const file = await findFile(HISTORY_FILE);

  if (file) {
    const remoteHistory = await readFile(file.id);
    const remoteEntries = Array.isArray(remoteHistory) ? remoteHistory : [];

    // Merge: union deduped by text (keep most recent timestamp per text)
    const merged = mergeHistories(localHistory, remoteEntries);

    // Save merged locally
    saveLocalHistory(merged);

    // Push merged to Drive
    await writeFile(HISTORY_FILE, merged, file.id);

    console.log(`[Sync] History merged: ${localHistory.length} local + ${remoteEntries.length} remote -> ${merged.length} merged`);
    return merged;
  } else {
    // No remote — push local
    if (localHistory.length > 0) {
      await writeFile(HISTORY_FILE, localHistory);
      console.log(`[Sync] History uploaded: ${localHistory.length} entries (first sync)`);
    }
    return localHistory;
  }
}

function mergeHistories(local, remote) {
  // Build a map keyed by normalized text — keep entry with most recent timestamp
  const map = new Map();

  for (const entry of [...local, ...remote]) {
    if (!entry || typeof entry.text !== 'string') continue;
    const key = entry.text.trim().toLowerCase();
    const existing = map.get(key);
    if (!existing || entry.time > existing.time) {
      map.set(key, entry);
    }
  }

  // Sort newest-first, cap at 50
  return Array.from(map.values())
    .sort((a, b) => b.time - a.time)
    .slice(0, 50);
}

// ── Public Sync API ────────────────────────────────────

export async function syncNow() {
  if (!refreshToken || syncInProgress) return;
  syncInProgress = true;
  emitStatus('syncing', 'Syncing\u2026');

  // Yield to the browser's rendering engine so the spinner paints
  // before the network calls start (critical in production builds)
  await new Promise(resolve => requestAnimationFrame(resolve));

  try {
    await ensureValidToken();
    const [dictResult, historyResult] = await Promise.all([
      syncDictionary(),
      syncHistory(),
    ]);

    emitStatus('synced', 'Synced');

    // Notify main.js to re-render if data changed
    window.dispatchEvent(new CustomEvent('sync-data-changed', {
      detail: { dictionary: dictResult, history: historyResult }
    }));

    return { dictionary: dictResult, history: historyResult };
  } catch (err) {
    console.error('[Sync] Failed:', err);
    emitStatus('error', 'Sync failed');
    throw err;
  } finally {
    syncInProgress = false;
  }
}

/**
 * Trigger sync after a local data change.
 * Debounced to avoid spamming Drive API.
 */
let debouncedSyncTimeout = null;
export function scheduleSyncAfterChange() {
  if (!refreshToken) return;
  if (debouncedSyncTimeout) clearTimeout(debouncedSyncTimeout);
  debouncedSyncTimeout = setTimeout(() => {
    syncNow().catch(err => console.warn('[Sync] Debounced sync failed:', err));
  }, 3000); // 3s debounce
}

// ── Auto-Sync Timer ────────────────────────────────────
// Sync is triggered per-write (dictionary add/remove, transcription saved)
// via scheduleSyncAfterChange() called from main.js.
// The daily heartbeat below is a safety net for long-running sessions.
const ONE_DAY_MS = 24 * 60 * 60 * 1000;

function startAutoSync() {
  stopAutoSync();
  syncTimer = setInterval(() => {
    syncNow().catch(err => console.warn('[Sync] Daily heartbeat sync failed:', err));
  }, ONE_DAY_MS);
}

function stopAutoSync() {
  if (syncTimer) {
    clearInterval(syncTimer);
    syncTimer = null;
  }
}

// ── Local Storage Accessors ────────────────────────────
// Dictionary: { terms: string[], deleted: string[] }
// Auto-migrates from old string[] format.

function getLocalDictionary() {
  try {
    const raw = JSON.parse(localStorage.getItem('annotate_dictionary') || '{"terms":[],"deleted":[]}');
    // Migrate from old string[] format
    if (Array.isArray(raw)) {
      return { terms: raw, deleted: [] };
    }
    return {
      terms: Array.isArray(raw.terms) ? raw.terms : [],
      deleted: Array.isArray(raw.deleted) ? raw.deleted : [],
    };
  } catch {
    return { terms: [], deleted: [] };
  }
}

function saveLocalDictionary(store) {
  localStorage.setItem('annotate_dictionary', JSON.stringify(store));
}

function getLocalHistory() {
  try {
    return JSON.parse(localStorage.getItem('annotate_history') || '[]');
  } catch {
    return [];
  }
}

function saveLocalHistory(history) {
  localStorage.setItem('annotate_history', JSON.stringify(history));
}
