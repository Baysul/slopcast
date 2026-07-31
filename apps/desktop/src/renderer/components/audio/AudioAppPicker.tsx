import type { AudioApp } from '@slopcast/shared-types';
import { Check } from 'lucide-react';
import type React from 'react';
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card';
import { type AudioAppGroup, groupAudioApps } from '../../utils/audio-grouping';
import { AudioLevelMeter } from './AudioLevelMeter';

export interface AudioAppPickerProps {
  audioApps: AudioApp[];
  audioAppGroups?: AudioAppGroup[];
  selectedAudioAppId: number | null;
  autoDetectedApp?: AudioApp | null;
  audioLevels: ReadonlyMap<number, number>;
  onSelectApp: (appId: number, explicit?: boolean) => void;
  onRefresh?: () => void;
  disabled?: boolean;
}

export const AudioAppPicker: React.FC<AudioAppPickerProps> = ({
  audioApps,
  audioAppGroups,
  selectedAudioAppId,
  onSelectApp,
  audioLevels,
  disabled = false,
}) => {
  const groups = audioAppGroups ?? groupAudioApps(audioApps);

  const getGroupLevel = (group: AudioAppGroup): number => {
    let max = 0;
    for (const member of group.members) {
      const lvl = audioLevels.get(member.id) ?? 0;
      if (lvl > max) max = lvl;
    }
    return max;
  };

  const getGroupTitle = (group: AudioAppGroup): string => {
    const rep = group.representative;
    if (rep.mediaTitle) return rep.mediaTitle;
    if (rep.windowTitle) return rep.windowTitle;
    return rep.name;
  };

  const getGroupSubtitle = (group: AudioAppGroup): string | null => {
    const rep = group.representative;
    if (rep.mediaTitle || rep.windowTitle) {
      return rep.name;
    }
    if (group.members.length > 1) {
      return `${group.members.length} audio streams`;
    }
    return null;
  };

  return (
    <Card className="border-border/60 bg-card/60 backdrop-blur-sm shadow-xl">
      <CardHeader className="pb-3">
        <CardTitle className="text-sm font-semibold flex items-center justify-between">
          <span>Audio Target</span>
          <span className="text-xs font-normal text-muted-foreground">
            {groups.length} active application{groups.length === 1 ? '' : 's'}
          </span>
        </CardTitle>
      </CardHeader>
      <CardContent>
        <div className="space-y-1.5 max-h-56 overflow-y-auto pr-1">
          {groups.map((group) => {
            const isSelected = selectedAudioAppId === group.representative.id;
            const level = getGroupLevel(group);
            const title = getGroupTitle(group);
            const subtitle = getGroupSubtitle(group);

            const cardStyle = isSelected
              ? 'bg-safelight/10 border-safelight/40 text-foreground'
              : 'bg-secondary/40 border-border/40 text-muted-foreground hover:bg-secondary/80 hover:text-foreground';

            return (
              <button
                type="button"
                key={group.representative.id}
                disabled={disabled}
                onClick={() => onSelectApp(group.representative.id, true)}
                className={`w-full text-left p-2.5 rounded-lg border transition-all flex items-center justify-between gap-3 text-xs focus:outline-none focus-visible:ring-2 focus-visible:ring-safelight/70 ${cardStyle} ${
                  disabled ? 'opacity-50 cursor-not-allowed' : 'cursor-pointer'
                }`}
              >
                <div className="flex items-center gap-2.5 min-w-0">
                  <span
                    className={`shrink-0 w-4 h-4 rounded-full flex items-center justify-center border ${
                      isSelected ? 'border-safelight bg-safelight text-background' : 'border-muted-foreground/30'
                    }`}
                  >
                    {isSelected && <Check className="w-2.5 h-2.5 stroke-[3]" aria-hidden="true" />}
                  </span>
                  <div className="min-w-0">
                    <p className="font-medium truncate leading-tight text-foreground">{title}</p>
                    {subtitle && <p className="text-[10px] text-muted-foreground truncate mt-0.5">{subtitle}</p>}
                  </div>
                </div>

                <div className="flex items-center gap-2 shrink-0">
                  <AudioLevelMeter level={level} />
                </div>
              </button>
            );
          })}
        </div>
      </CardContent>
    </Card>
  );
};
