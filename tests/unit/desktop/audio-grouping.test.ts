import assert from 'node:assert/strict';

import { test } from 'vite-plus/test';

import { audioAppsEqual, groupAudioApps } from '../../../apps/desktop/src/renderer/utils/audio-grouping.ts';

interface App {
  id: number;
  name: string;
  processId: number;
  bundleId?: string | null;
  windowTitle?: string | null;
  clientId?: number | null;
  mediaTitle?: string | null;
}

const app = (overrides: Partial<App> & { id: number }): App => ({
  bundleId: null,
  clientId: null,
  mediaTitle: null,
  name: 'Unnamed',
  processId: 0,
  windowTitle: null,
  ...overrides,
});

test('grouped by bundle id when present', () => {
  const apps = [
    app({ bundleId: 'com.spotify.client', id: 1, name: 'Spotify', processId: 100 }),
    app({ bundleId: 'com.spotify.client', id: 2, name: 'Spotify', processId: 200 }),
    app({ bundleId: 'org.videolan.vlc', id: 3, name: 'VLC' }),
  ];
  const groups = groupAudioApps(apps);
  assert.equal(groups.length, 2);
  const spotify = groups.find((g) => g.members.length === 2);
  assert.ok(spotify);
  assert.deepEqual(
    spotify.members.map((m) => m.id).sort((a, b) => a - b),
    [1, 2],
  );
});

test('grouped by name when bundle id is missing', () => {
  const apps = [app({ id: 1, name: 'Firefox', processId: 100 }), app({ id: 2, name: 'Firefox', processId: 200 })];
  const groups = groupAudioApps(apps);
  assert.equal(groups.length, 1);
  assert.equal(groups[0].members.length, 2);
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
    app({ clientId: 50, id: 1, name: '', processId: 0 }),
    app({ clientId: 50, id: 2, name: '', processId: 0 }),
    app({ clientId: null, id: 3, name: '', processId: 0 }),
  ];
  const groups = groupAudioApps(apps);
  assert.equal(groups.length, 2);
  assert.equal(groups[0].members.length, 2);
  assert.equal(groups[1].members.length, 1);
});

test('representative is promoted to the member with a media title', () => {
  const apps = [
    app({ id: 1, mediaTitle: null, name: 'Spotify', windowTitle: 'Spotify Free' }),
    app({ id: 2, mediaTitle: 'Artist - Track', name: 'Spotify', windowTitle: 'Spotify Free' }),
  ];
  const groups = groupAudioApps(apps);
  assert.equal(groups.length, 1);
  const group = groups.at(0);
  assert.ok(group !== undefined);
  assert.equal(group.representative.id, 2);
});

test('representative keeps first window title when no media title exists', () => {
  const apps = [
    app({ id: 1, mediaTitle: null, name: 'Firefox', windowTitle: 'Tab One' }),
    app({ id: 2, mediaTitle: null, name: 'Firefox', windowTitle: 'Tab Two' }),
  ];
  const groups = groupAudioApps(apps);
  const group = groups.at(0);
  assert.ok(group !== undefined);
  assert.equal(group.representative.id, 1);
});

test('existing media title is never replaced by a later member', () => {
  const apps = [
    app({ id: 1, mediaTitle: 'First', name: 'Spotify', windowTitle: null }),
    app({ id: 2, mediaTitle: 'Second', name: 'Spotify', windowTitle: 'T' }),
  ];
  const groups = groupAudioApps(apps);
  const group = groups.at(0);
  assert.ok(group !== undefined);
  assert.equal(group.representative.id, 1);
});

test('empty input yields no groups', () => {
  assert.deepEqual(groupAudioApps([]), []);
});

test('poll dedup keeps an unchanged list identical', () => {
  const list = [
    app({ id: 1, mediaTitle: 'Artist - Track', name: 'Spotify', windowTitle: 'Spotify Free' }),
    app({ id: 2, name: 'Firefox', windowTitle: 'Tab One' }),
  ];
  assert.equal(audioAppsEqual(list, [...list]), true);
});

test('poll dedup notices a media title change', () => {
  const before = [app({ id: 1, mediaTitle: 'Old Song', name: 'Spotify' })];
  const after = [app({ id: 1, mediaTitle: 'New Song', name: 'Spotify' })];
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
      [app({ id: 1, mediaTitle: null, name: 'Spotify' })],
      [app({ id: 1, mediaTitle: 'Song', name: 'Spotify' })],
    ),
    false,
  );
  assert.equal(
    audioAppsEqual(
      [app({ id: 1, mediaTitle: 'Song', name: 'Spotify' })],
      [app({ id: 1, mediaTitle: null, name: 'Spotify' })],
    ),
    false,
  );
});

test('poll dedup notices name and length changes', () => {
  assert.equal(audioAppsEqual([app({ id: 1, name: 'A' })], [app({ id: 1, name: 'B' })]), false);
  assert.equal(audioAppsEqual([app({ id: 1, name: 'A' })], []), false);
});
