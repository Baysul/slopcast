import assert from 'node:assert/strict';
import { test } from 'node:test';

import { type AudioAppGroup, audioAppsEqual, groupAudioApps } from '../src/renderer/utils/audio-grouping.ts';

interface App {
  id: number;
  name: string;
  processId: number;
  bundleId?: string | null;
  windowTitle?: string | null;
  clientId?: number | null;
  mediaTitle?: string | null;
}

function app(overrides: Partial<App> & { id: number }): App {
  return {
    name: 'Unnamed',
    processId: 0,
    bundleId: null,
    windowTitle: null,
    clientId: null,
    mediaTitle: null,
    ...overrides,
  };
}

test('grouped by bundle id when present', () => {
  const apps = [
    app({ id: 1, name: 'Spotify', bundleId: 'com.spotify.client', processId: 100 }),
    app({ id: 2, name: 'Spotify', bundleId: 'com.spotify.client', processId: 200 }),
    app({ id: 3, name: 'VLC', bundleId: 'org.videolan.vlc' }),
  ];
  const groups = groupAudioApps(apps);
  assert.equal(groups.length, 2);
  const spotify = groups.find((g) => g.members.length === 2);
  assert.ok(spotify);
  assert.deepEqual(spotify.members.map((m) => m.id).sort(), [1, 2]);
});

test('grouped by name when bundle id is missing', () => {
  const apps = [app({ id: 1, name: 'Firefox', processId: 100 }), app({ id: 2, name: 'Firefox', processId: 200 })];
  const groups = groupAudioApps(apps);
  assert.equal(groups.length, 1);
  assert.equal(groups[0]?.members.length, 2);
});

test('grouping keys are case and whitespace insensitive', () => {
  const apps = [app({ id: 1, name: '  Spotify ' }), app({ id: 2, name: 'spotify' })];
  const groups = groupAudioApps(apps);
  assert.equal(groups.length, 1);
});

test('same name different case groups; different names do not', () => {
  const apps = [app({ id: 1, name: 'VLC' }), app({ id: 2, name: 'Vlc' }), app({ id: 3, name: 'vlc-media-player' })];
  const groups = groupAudioApps(apps);
  assert.equal(groups.length, 2);
});

test('falls back to process id when name is blank', () => {
  const apps = [app({ id: 1, name: '', processId: 777 }), app({ id: 2, name: ' ', processId: 777 })];
  const groups = groupAudioApps(apps);
  assert.equal(groups.length, 1);
});

test('falls back to client id and then app id', () => {
  const apps = [
    app({ id: 1, name: '', processId: 0, clientId: 50 }),
    app({ id: 2, name: '', processId: 0, clientId: 50 }),
    app({ id: 3, name: '', processId: 0, clientId: null }),
  ];
  const groups = groupAudioApps(apps);
  assert.equal(groups.length, 2);
  assert.equal(groups[0]?.members.length, 2);
  assert.equal(groups[1]?.members.length, 1);
});

test('representative is promoted to the member with a media title', () => {
  const apps = [
    app({ id: 1, name: 'Spotify', mediaTitle: null, windowTitle: 'Spotify Free' }),
    app({ id: 2, name: 'Spotify', mediaTitle: 'Artist - Track', windowTitle: 'Spotify Free' }),
  ];
  const groups = groupAudioApps(apps);
  assert.equal(groups.length, 1);
  const group = groups[0] as AudioAppGroup;
  assert.equal(group.representative.id, 2);
});

test('representative keeps first window title when no media title exists', () => {
  const apps = [
    app({ id: 1, name: 'Firefox', windowTitle: 'Tab One', mediaTitle: null }),
    app({ id: 2, name: 'Firefox', windowTitle: 'Tab Two', mediaTitle: null }),
  ];
  const groups = groupAudioApps(apps);
  const group = groups[0] as AudioAppGroup;
  // First member stays representative: a later window title must not
  // override an existing one.
  assert.equal(group.representative.id, 1);
});

test('existing media title is never replaced by a later member', () => {
  const apps = [
    app({ id: 1, name: 'Spotify', mediaTitle: 'First', windowTitle: null }),
    app({ id: 2, name: 'Spotify', mediaTitle: 'Second', windowTitle: 'T' }),
  ];
  const groups = groupAudioApps(apps);
  const group = groups[0] as AudioAppGroup;
  assert.equal(group.representative.id, 1);
});

test('empty input yields no groups', () => {
  assert.deepEqual(groupAudioApps([]), []);
});

test('poll dedup keeps an unchanged list identical', () => {
  const list = [
    app({ id: 1, name: 'Spotify', mediaTitle: 'Artist - Track', windowTitle: 'Spotify Free' }),
    app({ id: 2, name: 'Firefox', windowTitle: 'Tab One' }),
  ];
  assert.equal(audioAppsEqual(list, [...list]), true);
});

test('poll dedup notices a media title change', () => {
  const before = [app({ id: 1, name: 'Spotify', mediaTitle: 'Old Song' })];
  const after = [app({ id: 1, name: 'Spotify', mediaTitle: 'New Song' })];
  assert.equal(audioAppsEqual(before, after), false);
});

test('poll dedup notices a window title change', () => {
  const before = [app({ id: 1, name: 'Firefox', windowTitle: 'Tab One' })];
  const after = [app({ id: 1, name: 'Firefox', windowTitle: 'Tab Two' })];
  assert.equal(audioAppsEqual(before, after), false);
});

test('poll dedup notices a title arriving or clearing', () => {
  assert.equal(
    audioAppsEqual(
      [app({ id: 1, name: 'Spotify', mediaTitle: null })],
      [app({ id: 1, name: 'Spotify', mediaTitle: 'Song' })],
    ),
    false,
  );
  assert.equal(
    audioAppsEqual(
      [app({ id: 1, name: 'Spotify', mediaTitle: 'Song' })],
      [app({ id: 1, name: 'Spotify', mediaTitle: null })],
    ),
    false,
  );
});

test('poll dedup notices name and length changes', () => {
  assert.equal(audioAppsEqual([app({ id: 1, name: 'A' })], [app({ id: 1, name: 'B' })]), false);
  assert.equal(audioAppsEqual([app({ id: 1, name: 'A' })], []), false);
});
