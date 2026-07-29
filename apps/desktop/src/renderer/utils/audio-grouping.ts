import type { AudioApp } from '@slopcast/shared-types';

export interface AudioAppGroup {
  representative: AudioApp;
  members: AudioApp[];
}

export function groupAudioApps(apps: AudioApp[]): AudioAppGroup[] {
  const groups: AudioAppGroup[] = [];
  const identityMap = new Map<string, AudioAppGroup>();
  for (const app of apps) {
    const key = app.clientId != null && app.clientId > 0 ? `c:${app.clientId}` : `n:${app.name.toLowerCase()}`;
    const existing = identityMap.get(key);
    if (existing) {
      existing.members.push(app);
      continue;
    }
    const group: AudioAppGroup = { representative: app, members: [app] };
    groups.push(group);
    identityMap.set(key, group);
  }
  return groups;
}
