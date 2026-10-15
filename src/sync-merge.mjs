const SYNC_SCHEMA_VERSION = 2;

export function normalizeKey(value) {
  return String(value ?? '').trim().toLowerCase();
}

function timestamp(value, fallback = 0) {
  const parsed = Number(value);
  return Number.isFinite(parsed) && parsed >= 0 ? parsed : fallback;
}

function dictionaryTerm(raw) {
  if (typeof raw === 'string') {
    const value = raw.trim();
    return value ? { value, updatedAt: 0 } : null;
  }
  if (!raw || typeof raw.value !== 'string') return null;
  const value = raw.value.trim();
  return value ? { value, updatedAt: timestamp(raw.updatedAt) } : null;
}

function dictionaryDeletion(raw) {
  if (typeof raw === 'string') {
    const value = raw.trim();
    return value ? { value, deletedAt: 0 } : null;
  }
  if (!raw || typeof raw.value !== 'string') return null;
  const value = raw.value.trim();
  return value ? { value, deletedAt: timestamp(raw.deletedAt) } : null;
}

function newestByKey(items, valueOf, timestampOf) {
  const result = new Map();
  for (const item of items) {
    if (!item) continue;
    const key = normalizeKey(valueOf(item));
    if (!key) continue;
    const current = result.get(key);
    if (!current || timestampOf(item) > timestampOf(current)) result.set(key, item);
  }
  return result;
}

export function normalizeDictionaryStore(raw) {
  const source = Array.isArray(raw) ? { terms: raw, deleted: [] } : (raw || {});
  const terms = newestByKey(
    (Array.isArray(source.terms) ? source.terms : []).map(dictionaryTerm),
    item => item.value,
    item => item.updatedAt,
  );
  const deleted = newestByKey(
    (Array.isArray(source.deleted) ? source.deleted : []).map(dictionaryDeletion),
    item => item.value,
    item => item.deletedAt,
  );

  for (const [key, deletion] of deleted) {
    const term = terms.get(key);
    if (term && deletion.deletedAt >= term.updatedAt) terms.delete(key);
    else if (term) deleted.delete(key);
  }

  return {
    version: SYNC_SCHEMA_VERSION,
    terms: [...terms.values()].sort((a, b) => a.value.localeCompare(b.value, undefined, { sensitivity: 'base' })),
    deleted: [...deleted.values()],
  };
}

export function mergeDictionaries(localRaw, remoteRaw) {
  const local = normalizeDictionaryStore(localRaw);
  const remote = normalizeDictionaryStore(remoteRaw);
  return normalizeDictionaryStore({
    version: SYNC_SCHEMA_VERSION,
    terms: [...local.terms, ...remote.terms],
    deleted: [...local.deleted, ...remote.deleted],
  });
}

function historyEntry(raw) {
  if (!raw || typeof raw.text !== 'string') return null;
  const text = raw.text.trim();
  if (!text) return null;
  const time = timestamp(raw.time);
  return { text, time, updatedAt: timestamp(raw.updatedAt, time) };
}

function historyDeletion(raw) {
  if (!raw || typeof raw.text !== 'string') return null;
  const text = raw.text.trim();
  if (!text) return null;
  const time = timestamp(raw.time);
  return { text, time, deletedAt: timestamp(raw.deletedAt, time) };
}

export function normalizeHistoryStore(raw) {
  const source = Array.isArray(raw) ? { entries: raw, deleted: [] } : (raw || {});
  const entries = newestByKey(
    (Array.isArray(source.entries) ? source.entries : []).map(historyEntry),
    item => item.text,
    item => item.updatedAt,
  );
  const deleted = newestByKey(
    (Array.isArray(source.deleted) ? source.deleted : []).map(historyDeletion),
    item => item.text,
    item => item.deletedAt,
  );

  for (const [key, deletion] of deleted) {
    const entry = entries.get(key);
    if (entry && deletion.deletedAt >= entry.updatedAt) entries.delete(key);
    else if (entry) deleted.delete(key);
  }

  return {
    version: SYNC_SCHEMA_VERSION,
    entries: [...entries.values()]
      .sort((a, b) => b.time - a.time)
      .slice(0, 50),
    deleted: [...deleted.values()],
  };
}

export function mergeHistories(localRaw, remoteRaw) {
  const local = normalizeHistoryStore(localRaw);
  const remote = normalizeHistoryStore(remoteRaw);
  return normalizeHistoryStore({
    version: SYNC_SCHEMA_VERSION,
    entries: [...local.entries, ...remote.entries],
    deleted: [...local.deleted, ...remote.deleted],
  });
}
