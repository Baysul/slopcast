import assert from 'node:assert/strict';
import { test } from 'node:test';

import {
  isGroupSelected,
  NO_AUDIO_APP,
  selectedTargetProcessId,
  toAudioTargetId,
} from '../../../apps/desktop/src/renderer/utils/audio-targets.ts';

interface App {
  id: number;
  name: string;
  processId: number;
  bundleId?: string | null;
  windowTitle?: string | null;
  clientId?: number | null;
  mediaTitle?: string | null;
}

const app = (id: number, name: string, processId: number): App => ({ id, name, processId });

test('linux stream entry encodes as negative pid', () => {
  assert.equal(toAudioTargetId(app(148, 'Chromium', 174581), true), -174581);
});

test('linux idle entry keeps its negative pid id', () => {
  assert.equal(toAudioTargetId(app(-103906, 'Firefox', 103906), true), -103906);
});

test('linux entries with pid 0 keep their node id', () => {
  assert.equal(toAudioTargetId(app(85, 'ZenlessZoneZero.exe', 0), true), 85);
});

test('windows entries keep their positive pid id', () => {
  assert.equal(toAudioTargetId(app(1234, 'Spotify', 1234), false), 1234);
});

test('system audio keeps -1 on every platform', () => {
  assert.equal(toAudioTargetId(app(-1, 'System Audio', 0), true), -1);
  assert.equal(toAudioTargetId(app(-1, 'System Audio', 0), false), -1);
});

test('no audio sentinel is id 0', () => {
  assert.equal(NO_AUDIO_APP.id, 0);
  assert.equal(NO_AUDIO_APP.name, 'No Audio');
});

test('selectedTargetProcessId interprets only encodings below -1 as pids', () => {
  assert.equal(selectedTargetProcessId(-174581), 174581);
  assert.equal(selectedTargetProcessId(-1), null);
  assert.equal(selectedTargetProcessId(0), null);
  assert.equal(selectedTargetProcessId(148), null);
  assert.equal(selectedTargetProcessId(null), null);
});

test('isGroupSelected matches a negative pid selection by processId', () => {
  const playing = [app(148, 'Chromium', 174581)];
  const paused = [app(-174581, 'Chromium', 174581)];
  assert.equal(isGroupSelected(playing, -174581), true);
  assert.equal(isGroupSelected(paused, -174581), true);
});

test('isGroupSelected falls back to exact id for node/pid-0 selections', () => {
  const nodeEntry = [app(85, 'ZenlessZoneZero.exe', 0)];
  assert.equal(isGroupSelected(nodeEntry, 85), true);
  assert.equal(isGroupSelected(nodeEntry, -85), false);
  assert.equal(isGroupSelected([app(-103906, 'Firefox', 103906)], -103907), false);
});

test('isGroupSelected never matches a different app with same process', () => {
  const group = [app(-103906, 'Firefox', 103906)];
  assert.equal(isGroupSelected(group, -999), false);
});
