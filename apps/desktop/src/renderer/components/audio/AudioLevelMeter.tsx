import { type AudioLevelSubscription, AudioVisualizer } from '@slopcast/ui';
import type React from 'react';
import { useMemo } from 'react';
import { audioWaveStore, silentWave, WAVE_COLUMN_COUNT } from '../../utils/audio-level-store';

const DEFAULT_WIDTH = 96;
const DEFAULT_HEIGHT = 20;

export interface AudioLevelMeterProps {
  appId?: number;
  memberIds?: number[];
  width?: number;
  height?: number;
  className?: string;
}

const resolveIds = (appId?: number, memberIds?: number[]): number[] => {
  if (memberIds && memberIds.length > 0) return memberIds;
  if (appId !== undefined) return [appId];
  return [];
};

const unionColumns = (merged: number[], columns: number[]): void => {
  const pairs = Math.min(Math.floor(columns.length / 2), WAVE_COLUMN_COUNT);
  for (let index = 0; index < pairs * 2; index += 1) {
    const level = columns[index] ?? 0;
    const current = merged[index] ?? 0;
    if (index % 2 === 0 && level < current) merged[index] = level;
    if (index % 2 !== 0 && level > current) merged[index] = level;
  }
};

const mergeWaves = (members: ReadonlyMap<number, number[]>, merged: number[]): void => {
  merged.fill(0);
  let isFirstWave = true;
  for (const columns of members.values()) {
    if (isFirstWave) {
      const columnCount = Math.min(columns.length, merged.length);
      for (let index = 0; index < columnCount; index += 1) {
        merged[index] = columns[index] ?? 0;
      }
      isFirstWave = false;
      continue;
    }
    unionColumns(merged, columns);
  }
};

const createSubscription =
  (ids: readonly number[]): AudioLevelSubscription =>
  (listener) => {
    const memberWaves = new Map<number, number[]>();
    const merged = silentWave();
    const unsubscribe = ids.map((id) =>
      audioWaveStore.subscribe(id, (columns) => {
        memberWaves.set(id, columns);
        mergeWaves(memberWaves, merged);
        listener(merged);
      }),
    );
    return () => {
      for (const stopListening of unsubscribe) stopListening();
    };
  };

export const AudioLevelMeter: React.FC<AudioLevelMeterProps> = ({
  appId,
  memberIds,
  width = DEFAULT_WIDTH,
  height = DEFAULT_HEIGHT,
  className = '',
}) => {
  const idsKey = resolveIds(appId, memberIds).join(',');
  const subscribe = useMemo(() => createSubscription(idsKey === '' ? [] : idsKey.split(',').map(Number)), [idsKey]);

  return (
    <AudioVisualizer
      subscribe={subscribe}
      width={width}
      height={height}
      hideWhenSilent
      className={`shrink-0 ${className}`}
    />
  );
};
