import type { AudioApp } from '@slopcast/shared-types';

export interface AudioAppGroup {
  representative: AudioApp;
  members: AudioApp[];
}

const identityKeyFor = (app: AudioApp): string => {
  if (app.bundleId && app.bundleId.trim().length > 0) {
    return `b:${app.bundleId.trim().toLowerCase()}`;
  }
  if (app.name && app.name.trim().length > 0) {
    return `n:${app.name.trim().toLowerCase()}`;
  }
  if (app.processId > 0) {
    return `p:${app.processId}`;
  }
  if (app.clientId != null && app.clientId > 0) {
    return `c:${app.clientId}`;
  }
  return `i:${app.id}`;
};

const mergeIntoGroup = (group: AudioAppGroup, app: AudioApp): void => {
  group.members.push(app);
  if (!group.representative.mediaTitle && app.mediaTitle) {
    group.representative = app;
  } else if (!group.representative.mediaTitle && !group.representative.windowTitle && app.windowTitle) {
    group.representative = app;
  }
};

export function groupAudioApps(apps: AudioApp[]): AudioAppGroup[] {
  const groups: AudioAppGroup[] = [];
  const identityMap = new Map<string, AudioAppGroup>();
  for (const app of apps) {
    const key = identityKeyFor(app);
    const existing = identityMap.get(key);
    if (existing) {
      mergeIntoGroup(existing, app);
      continue;
    }
    const group: AudioAppGroup = { representative: app, members: [app] };
    groups.push(group);
    identityMap.set(key, group);
  }
  return groups;
}
