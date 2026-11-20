import test from 'node:test';
import assert from 'node:assert/strict';
import { appendFileChunk, DOWNLOAD_LIMIT } from './storage';
const file = { index: 1, name: 'TEST.TXT', size: 129, directory: false };
const chunk = {
  index: 1,
  offset: 0,
  total_size: 129,
  data: Array<number>(128).fill(7),
  eof: false,
};
test('sequential 128-byte file chunks, EOF and empty files', () => {
  const bytes: number[] = [];
  assert.equal(appendFileChunk(file, bytes, chunk), false);
  assert.equal(appendFileChunk(file, bytes, { ...chunk, offset: 128, data: [8], eof: true }), true);
  assert.equal(bytes.length, 129);
  assert.equal(
    appendFileChunk({ ...file, size: 0 }, [], { ...chunk, total_size: 0, data: [], eof: true }),
    true,
  );
});
test('rejects unmatched, empty, premature EOF, oversized and changed-size chunks without appending', () => {
  for (const patch of [
    { index: 2 },
    { offset: 128 },
    { total_size: 130 },
    { data: [] },
    { eof: true },
    { data: Array(129).fill(0) },
  ]) {
    const bytes: number[] = [];
    assert.throws(() => appendFileChunk(file, bytes, { ...chunk, ...patch }), /Invalid/);
    assert.equal(bytes.length, 0);
  }
});
test('larger files stop exactly at 64 KiB without requiring EOF', () => {
  const bytes: number[] = [];
  const large = { ...file, size: DOWNLOAD_LIMIT + 1 };
  while (bytes.length < DOWNLOAD_LIMIT) {
    const last = bytes.length + 128 === DOWNLOAD_LIMIT;
    assert.equal(
      appendFileChunk(large, bytes, { ...chunk, total_size: large.size, offset: bytes.length }),
      last,
    );
  }
  assert.equal(bytes.length, DOWNLOAD_LIMIT);
});
