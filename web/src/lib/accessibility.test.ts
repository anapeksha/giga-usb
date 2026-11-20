import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';
import { summary, type ProtocolRecord } from './protocol';

const source = (path: string) => readFileSync(new URL(`../${path}`, import.meta.url), 'utf8');

// Source-level guards complement, rather than replace, a rendered accessibility audit.
test('dashboard headings, status list and descriptor tables retain native semantics', () => {
  assert.match(source('components/ui/card.tsx'), /<h2\b/);
  assert.doesNotMatch(source('components/topology.tsx'), /<h4\b/);
  assert.match(source('App.tsx'), /<dl class="status-grid">/);
  assert.match(source('components/topology.tsx'), /<caption\b/);
  assert.match(source('components/topology.tsx'), /<TableHead scope="col">/);
  assert.match(source('styles.css'), /summary:focus-visible/);
});

test('Wi-Fi validation is associated with fields and survives routine polling', () => {
  const wifi = source('components/wifi.tsx');
  for (const field of ['ssid', 'password']) {
    assert.match(wifi, new RegExp(`aria-invalid=\\{invalidField\\(\\) === '${field}'`));
  }
  assert.equal((wifi.match(/wifi-help wifi-notice/g) ?? []).length, 2);
  assert.match(wifi, /id="wifi-notice"/);
  assert.match(wifi, /if \(handoff\(\) && next.mode !== 'connecting'\)/);
  assert.match(wifi, /passwordInput\)\?\.focus\(\)/);
});

test('high-frequency trace and counters are not live announcements', () => {
  assert.doesNotMatch(source('components/trace.tsx'), /aria-live|role="(?:status|alert)"/);
  assert.match(source('components/trace.tsx'), /role="region"/);
  assert.match(source('App.tsx'), /aria-label=\{`Download \$\{file.name\}`\}/);
});

test('normal trace summaries omit packet encoding while raw records retain it', () => {
  const record: ProtocolRecord = {
    timestamp_ms: 12,
    event: {
      type: 'host',
      event: {
        transfer: { direction: 'in', endpoint: 129, bytes: 2, pid: 2, data: [1, 2] },
      },
    },
  };
  assert.equal(summary(record), 'Received · endpoint 129');
  assert.match(JSON.stringify(record), /"pid":2/);
  assert.match(source('components/trace.tsx'), /JSON.stringify\(props.record, null, 2\)/);
});
