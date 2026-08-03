import assert from 'node:assert/strict';
import { test } from 'node:test';

import {
  audioWaveStore,
  silentWave,
  WAVE_COLUMN_COUNT,
  waveIsActive,
} from '../src/renderer/utils/audio-level-store.ts';

const SILENT = silentWave();

test('silent wave is 192 zeroed columns', () => {
  assert.equal(SILENT.length, WAVE_COLUMN_COUNT * 2);
  assert.ok(SILENT.every((v) => v === 0));
});

test('waveIsActive only flags columns above the activity threshold', () => {
  assert.equal(waveIsActive(silentWave()), false);
  assert.equal(waveIsActive([0.001, -0.001, 0, 0]), false);
  assert.equal(waveIsActive([0, 0.01, 0, 0]), true);
  assert.equal(waveIsActive([0, 0, -0.01, 0]), true);
});

test('subscribe emits the current wave immediately', () => {
  const seen: number[][] = [];
  audioWaveStore.subscribe(1, (columns) => seen.push(columns));
  assert.equal(seen.length, 1);
  assert.deepEqual(seen[0], silentWave());
});

test('updateWave notifies subscribers with new columns', () => {
  const seen: number[][] = [];
  audioWaveStore.subscribe(1, (columns) => seen.push(columns));
  const wave = silentWave();
  wave[0] = 0.5;
  audioWaveStore.updateWave([{ id: 1, columns: wave }]);
  assert.equal(seen.length, 2);
  assert.deepEqual(seen[1], wave);
});

test('subscribers are not notified for epsilon-level changes', () => {
  const seen: number[][] = [];
  audioWaveStore.subscribe(2, (columns) => seen.push(columns));
  const wave = silentWave();
  wave[0] = 0.001; // below WAVE_EPSILON
  audioWaveStore.updateWave([{ id: 2, columns: wave }]);
  // First update establishes the wave (one notification)...
  assert.equal(seen.length, 2);
  audioWaveStore.updateWave([{ id: 2, columns: wave }]);
  // ...but an identical (sub-epsilon) re-update must not notify again.
  assert.equal(seen.length, 2);
});

test('unsubscribing stops notifications', () => {
  const seen: number[][] = [];
  const unsubscribe = audioWaveStore.subscribe(3, (columns) => seen.push(columns));
  unsubscribe();
  audioWaveStore.updateWave([{ id: 3, columns: [1] }]);
  assert.equal(seen.length, 1);
});

test('apps missing from an update are reset to silence', () => {
  const seen: number[][] = [];
  audioWaveStore.subscribe(4, (columns) => seen.push(columns));
  audioWaveStore.updateWave([{ id: 4, columns: [0.5, 0.5] }]);
  audioWaveStore.updateWave([]);
  assert.equal(seen.length, 3);
  assert.deepEqual(seen[2], silentWave());
});

test('desktop audio (-1) mirrors the max pair across active apps', () => {
  const seen: number[][] = [];
  audioWaveStore.subscribe(-1, (columns) => seen.push(columns));
  const waveA = silentWave();
  waveA[0] = -0.4;
  waveA[1] = 0.3;
  const waveB = silentWave();
  waveB[0] = -0.2;
  waveB[1] = 0.6;
  audioWaveStore.updateWave([
    { id: 10, columns: waveA },
    { id: 11, columns: waveB },
  ]);
  const max = seen.at(-1) ?? [];
  assert.equal(max[0], -0.4);
  assert.equal(max[1], 0.6);
});

test('silent streams do not contribute to the desktop-audio max', () => {
  const seen: number[][] = [];
  audioWaveStore.subscribe(-1, (columns) => seen.push(columns));
  const silent = silentWave();
  const loud = silentWave();
  loud[0] = 0.1;
  loud[1] = 0.9;
  audioWaveStore.updateWave([
    { id: 20, columns: silent },
    { id: 21, columns: loud },
  ]);
  const max = seen.at(-1) ?? [];
  assert.equal(max[1], 0.9);
});

test('getWave returns silence for unknown ids', () => {
  assert.deepEqual(audioWaveStore.getWave(9999), silentWave());
});
