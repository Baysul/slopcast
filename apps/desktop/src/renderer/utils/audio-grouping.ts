import type { AudioApp } from '@slopcast/shared-types';

export interface AudioAppGroup {
  representative: AudioApp;
  members: AudioApp[];
}

export function groupAudioApps(apps: AudioApp[]): AudioAppGroup[] {
  const groups: AudioAppGroup[] = [];
  const identityMap = new Map<string, AudioAppGroup>();
  for (const app of apps) {
    let key: string;
    if (app.bundleId && app.bundleId.trim().length > 0) {
      key = `b:${app.bundleId.trim().toLowerCase()}`;
    } else if (app.name && app.name.trim().length > 0) {
      key = `n:${app.name.trim().toLowerCase()}`;
    } else if (app.processId > 0) {
      key = `p:${app.processId}`;
    } else if (app.clientId != null && app.clientId > 0) {
      key = `c:${app.clientId}`;
    } else {
      key = `i:${app.id}`;
    }

    const existing = identityMap.get(key);
    if (existing) {
      existing.members.push(app);
      if (!existing.representative.mediaTitle && app.mediaTitle) {
        existing.representative = app;
      } else if (!existing.representative.mediaTitle && !existing.representative.windowTitle && app.windowTitle) {
        existing.representative = app;
      }
      continue;
    }
    const group: AudioAppGroup = { representative: app, members: [app] };
    groups.push(group);
    identityMap.set(key, group);
  }
  return groups;
}
