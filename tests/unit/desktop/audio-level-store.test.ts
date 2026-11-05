import assert from 'node:assert/strict';

import { test } from 'vite-plus/test';

import {
  WAVE_COLUMN_COUNT,
  audioWaveStore,
  silentWave,
  waveIsActive,
} from '../../../apps/desktop/src/renderer/utils/audio-level-store.ts';

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
  audioWaveStore.subscribe(1, (columns) => {
    seen.push(columns);
  });
  assert.equal(seen.length, 1);
  assert.deepEqual(seen[0], silentWave());
});

test('updateWave notifies subscribers with new columns', () => {
  const seen: number[][] = [];
  audioWaveStore.subscribe(1, (columns) => {
    seen.push(columns);
  });
  const wave = silentWave();
  wave[0] = 0.5;
  audioWaveStore.updateWave([{ columns: wave, id: 1 }]);
  assert.equal(seen.length, 2);
  assert.deepEqual(seen[1], wave);
});

test('subscribers are not notified for epsilon-level changes', () => {
  const seen: number[][] = [];
  audioWaveStore.subscribe(2, (columns) => {
    seen.push(columns);
  });
  const wave = silentWave();
  wave[0] = 0.001;
  audioWaveStore.updateWave([{ columns: wave, id: 2 }]);
  assert.equal(seen.length, 2);
  audioWaveStore.updateWave([{ columns: wave, id: 2 }]);
  assert.equal(seen.length, 2);
});

test('unsubscribing stops notifications', () => {
  const seen: number[][] = [];
  const unsubscribe = audioWaveStore.subscribe(3, (columns) => {
    seen.push(columns);
  });
  unsubscribe();
  audioWaveStore.updateWave([{ columns: [1], id: 3 }]);
  assert.equal(seen.length, 1);
});

test('apps missing from an update are reset to silence', () => {
  const seen: number[][] = [];
  audioWaveStore.subscribe(4, (columns) => {
    seen.push(columns);
  });
  audioWaveStore.updateWave([{ columns: [0.5, 0.5], id: 4 }]);
  audioWaveStore.updateWave([]);
  assert.equal(seen.length, 3);
  assert.deepEqual(seen[2], silentWave());
});

test('desktop audio (-1) mirrors the max pair across active apps', () => {
  const seen: number[][] = [];
  audioWaveStore.subscribe(-1, (columns) => {
    seen.push(columns);
  });
  const waveA = silentWave();
  waveA[0] = -0.4;
  waveA[1] = 0.3;
  const waveB = silentWave();
  waveB[0] = -0.2;
  waveB[1] = 0.6;
  audioWaveStore.updateWave([
    { columns: waveA, id: 10 },
    { columns: waveB, id: 11 },
  ]);
  const max = seen.at(-1) ?? [];
  assert.equal(max[0], -0.4);
  assert.equal(max[1], 0.6);
});

test('silent streams do not contribute to the desktop-audio max', () => {
  const seen: number[][] = [];
  audioWaveStore.subscribe(-1, (columns) => {
    seen.push(columns);
  });
  const silent = silentWave();
  const loud = silentWave();
  loud[0] = 0.1;
  loud[1] = 0.9;
  audioWaveStore.updateWave([
    { columns: silent, id: 20 },
    { columns: loud, id: 21 },
  ]);
  const max = seen.at(-1) ?? [];
  assert.equal(max[1], 0.9);
});

test('getWave returns silence for unknown ids', () => {
  assert.deepEqual(audioWaveStore.getWave(9999), silentWave());
});

test('removed streams emit silence once and can return with the same id', () => {
  const seen: number[][] = [];
  const unsubscribe = audioWaveStore.subscribe(1001, (columns) => {
    seen.push(columns);
  });
  try {
    audioWaveStore.updateWave([{ id: 1001, columns: [0.5, -0.5] }]);
    audioWaveStore.updateWave([]);
    const countAfterRemoval = seen.length;
    assert.deepEqual(seen.at(-1), silentWave());
    assert.deepEqual(audioWaveStore.getWave(1001), silentWave());

    audioWaveStore.updateWave([]);
    assert.equal(seen.length, countAfterRemoval);
    audioWaveStore.updateWave([{ id: 1001, columns: [0.5, -0.5] }]);
    assert.deepEqual(seen.at(-1), [0.5, -0.5]);
    assert.equal(seen.length, countAfterRemoval + 1);
  } finally {
    unsubscribe();
  }
});
