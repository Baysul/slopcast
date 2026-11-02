import type { AudioApp } from '@slopcast/shared-types';

export interface AudioAppGroup {
  representative: AudioApp;
  members: AudioApp[];
}

const identityKeyFor = (app: AudioApp): string => {
  if (app.bundleId !== null && app.bundleId !== undefined && app.bundleId.trim().length > 0) {
    return `b:${app.bundleId.trim().toLowerCase()}`;
  }
  if (app.name.trim().length > 0) {
    return `n:${app.name.trim().toLowerCase()}`;
  }
  if (app.processId > 0) {
    return `p:${app.processId}`;
  }
  if (app.clientId !== null && app.clientId !== undefined && app.clientId > 0) {
    return `c:${app.clientId}`;
  }
  return `i:${app.id}`;
};

const hasTitle = (title: string | null | undefined): boolean => title !== null && title !== undefined && title !== '';

const mergeIntoGroup = (group: AudioAppGroup, app: AudioApp): void => {
  group.members.push(app);
  if (!hasTitle(group.representative.mediaTitle) && hasTitle(app.mediaTitle)) {
    group.representative = app;
  } else if (
    !hasTitle(group.representative.mediaTitle) &&
    !hasTitle(group.representative.windowTitle) &&
    hasTitle(app.windowTitle)
  ) {
    group.representative = app;
  }
};

export const groupAudioApps = (apps: AudioApp[]): AudioAppGroup[] => {
  const groups: AudioAppGroup[] = [];
  const identityMap = new Map<string, AudioAppGroup>();
  for (const app of apps) {
    const key = identityKeyFor(app);
    const existing = identityMap.get(key);
    if (existing) {
      mergeIntoGroup(existing, app);
    } else {
      const group: AudioAppGroup = { members: [app], representative: app };
      groups.push(group);
      identityMap.set(key, group);
    }
  }
  return groups;
};

export const audioAppsEqual = (a: readonly AudioApp[], b: readonly AudioApp[]): boolean => {
  if (a.length !== b.length) {
    return false;
  }
  return a.every((app, i) => {
    const other = b[i];
    return (
      other !== undefined &&
      app.id === other.id &&
      app.processId === other.processId &&
      (app.clientId ?? null) === (other.clientId ?? null) &&
      (app.bundleId ?? null) === (other.bundleId ?? null) &&
      app.name === other.name &&
      (app.mediaTitle ?? null) === (other.mediaTitle ?? null) &&
      (app.windowTitle ?? null) === (other.windowTitle ?? null)
    );
  });
};
