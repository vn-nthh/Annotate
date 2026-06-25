import test from 'node:test';
import assert from 'node:assert/strict';

import {
  mergeDictionaries,
  mergeHistories,
  normalizeDictionaryStore,
  normalizeHistoryStore,
} from '../src/sync-merge.mjs';

test('migrates old dictionary data and preserves delete-wins ties', () => {
  const migrated = normalizeDictionaryStore({ terms: ['Alpha'], deleted: ['Alpha'] });
  assert.deepEqual(migrated.terms, []);
  assert.equal(migrated.deleted[0].value, 'Alpha');
});

test('a newer dictionary re-add overrides an older remote tombstone', () => {
  const merged = mergeDictionaries(
    { terms: [{ value: 'Alpha', updatedAt: 20 }], deleted: [] },
    { terms: [], deleted: [{ value: 'Alpha', deletedAt: 10 }] },
  );
  assert.deepEqual(merged.terms, [{ value: 'Alpha', updatedAt: 20 }]);
  assert.deepEqual(merged.deleted, []);
});

test('a newer dictionary deletion overrides a stale remote value', () => {
  const merged = mergeDictionaries(
    { terms: [], deleted: [{ value: 'Alpha', deletedAt: 20 }] },
    { terms: [{ value: 'Alpha', updatedAt: 10 }], deleted: [] },
  );
  assert.deepEqual(merged.terms, []);
  assert.deepEqual(merged.deleted, [{ value: 'Alpha', deletedAt: 20 }]);
});

test('history re-add and delete resolution uses operation timestamps', () => {
  const readded = mergeHistories(
    { entries: [{ text: 'Hello', time: 30, updatedAt: 30 }], deleted: [] },
    { entries: [], deleted: [{ text: 'Hello', time: 10, deletedAt: 20 }] },
  );
  assert.equal(readded.entries[0].text, 'Hello');
  assert.deepEqual(readded.deleted, []);

  const deleted = normalizeHistoryStore({
    entries: [{ text: 'Hello', time: 30, updatedAt: 30 }],
    deleted: [{ text: 'Hello', time: 30, deletedAt: 40 }],
  });
  assert.deepEqual(deleted.entries, []);
  assert.equal(deleted.deleted[0].deletedAt, 40);
});
