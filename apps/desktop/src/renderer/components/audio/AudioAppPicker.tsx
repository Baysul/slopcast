import type { AudioApp } from '@slopcast/shared-types';
import { AudioLines, Check, Volume2, VolumeX } from 'lucide-react';
import React from 'react';
import { Button } from '@/components/ui/button';
import { type AudioAppGroup, groupAudioApps } from '../../utils/audio-grouping';
import { isGroupSelected, NO_AUDIO_APP } from '../../utils/audio-targets';
import { AudioLevelMeter } from './AudioLevelMeter';

export interface AudioAppPickerProps {
  audioApps: AudioApp[];
  audioAppGroups?: AudioAppGroup[];
  selectedAudioAppId: number | null;
  autoSelectAudio: boolean;
  isMuted: boolean;
  onToggleMute: () => void;
  autoDetectedApp?: AudioApp | null;
  onSelectApp: (appId: number | null, explicit?: boolean) => void;
  onRefresh?: () => void;
  disabled?: boolean;
}

const DESKTOP_AUDIO_APP: AudioApp = {
  id: -1,
  name: 'Desktop Audio (All System Sound)',
  processId: 0,
  clientId: null,
  mediaTitle: null,
};

const DESKTOP_AUDIO_GROUP: AudioAppGroup = {
  representative: DESKTOP_AUDIO_APP,
  members: [DESKTOP_AUDIO_APP],
};

export const AudioAppPicker: React.FC<AudioAppPickerProps> = React.memo(
  ({
    audioApps,
    audioAppGroups,
    selectedAudioAppId,
    autoDetectedApp,
    onSelectApp,
    onRefresh,
    autoSelectAudio,
    isMuted,
    onToggleMute,
    disabled = false,
  }) => {
    const groups = audioAppGroups ?? groupAudioApps(audioApps);
    const MuteIcon = isMuted ? VolumeX : Volume2;
    let selectionHint = 'Automatic audio selection is off. No Audio is selected until you choose an app.';
    if (autoSelectAudio)
      selectionHint = 'Audio is matched to your shared application when possible. Unmatched sources stay video-only.';

    const displayName = (app: AudioApp): string => app.name;

    const firstMemberTitle = (members: AudioApp[], key: 'mediaTitle' | 'windowTitle'): string | undefined => {
      for (const m of members) {
        if (m[key]) {
          return m[key];
        }
      }
      return undefined;
    };

    const groupSubLabel = (group: AudioAppGroup, isDesktopAudio: boolean): string | null => {
      if (isDesktopAudio) return 'All system audio';
      const { members } = group;
      const label = firstMemberTitle(members, 'mediaTitle') ?? firstMemberTitle(members, 'windowTitle');
      if (label) {
        if (members.length > 1) return `${label} \u00B7 ${members.length} streams`;
        return label;
      }
      if (members.length > 1) return `${members.length} audio streams`;
      return null;
    };

    const pickerRowClass = (isSelected: boolean): string => {
      if (isSelected) {
        return 'bg-safelight-glow border-safelight/30 text-safelight';
      }
      return 'bg-background/60 border-border hover:border-input hover:text-foreground';
    };

    const renderBtn = (group: AudioAppGroup) => {
      const { representative, members } = group;
      const isDesktopAudio = representative.id === -1;
      const isSelected = isGroupSelected(members, selectedAudioAppId);
      const isAutoDetected = members.some((m) => m.id === autoDetectedApp?.id);
      const memberIds = members.map((m) => m.id);
      const btnClass = `flex items-center justify-between p-3 rounded-lg border transition-all cursor-pointer text-left w-full focus:outline-none focus-visible:ring-2 focus-visible:ring-safelight/70 focus-visible:ring-offset-2 focus-visible:ring-offset-background ${pickerRowClass(
        isSelected,
      )} ${disabled ? 'opacity-50 cursor-not-allowed' : ''}`;

      return (
        <button
          key={representative.id}
          type="button"
          disabled={disabled}
          aria-pressed={isSelected}
          onClick={() => {
            if (representative.id === 0) {
              onSelectApp(0, true);
            } else if (isSelected) {
              onSelectApp(autoSelectAudio ? null : 0, false);
            } else {
              onSelectApp(representative.id, true);
            }
          }}
          className={btnClass}
        >
          <div className="min-w-0 flex-1">
            <div className="flex items-center gap-2">
              <span className="text-sm font-semibold truncate min-w-0">{displayName(representative)}</span>
              <AudioLevelMeter appId={representative.id} memberIds={memberIds} />
            </div>
            {(() => {
              const label = groupSubLabel(group, isDesktopAudio);
              if (!label) return null;
              return (
                <div className="flex items-center gap-2 mt-0.5">
                  <span className="text-xs opacity-60">{label}</span>
                  {isAutoDetected && (
                    <span className="text-xs bg-safelight-glow text-safelight/80 px-2 py-1 rounded-full">auto</span>
                  )}
                </div>
              );
            })()}
          </div>
          {isSelected && <Check className="w-4 h-4 shrink-0 text-safelight" aria-hidden="true" />}
        </button>
      );
    };

    return (
      <section aria-labelledby="audio-heading" className="space-y-4 p-5">
        <div className="flex items-center justify-between">
          <h2
            id="audio-heading"
            className="flex items-center gap-2.5 text-xs font-semibold uppercase tracking-wider text-muted-foreground"
          >
            <span className="grid size-7 place-items-center rounded-md bg-safelight-glow text-safelight">
              <AudioLines className="size-4" aria-hidden="true" />
            </span>
            Window Audio Capture
          </h2>
          {onRefresh && (
            <Button
              variant="ghost"
              size="sm"
              disabled={disabled}
              onClick={onRefresh}
              className="text-xs text-muted-foreground hover:text-foreground h-auto px-2"
            >
              Refresh
            </Button>
          )}
        </div>
        <p className="text-sm text-muted-foreground leading-relaxed">
          {selectionHint} Select an app to share only its sound, or choose{' '}
          <strong className="text-foreground">Desktop Audio</strong> explicitly to capture all system sound.
        </p>

        <Button
          variant="outline"
          aria-pressed={isMuted}
          disabled={disabled}
          onClick={onToggleMute}
          className="w-full justify-start"
        >
          <MuteIcon aria-hidden="true" />
          {isMuted ? 'Unmute audio source' : 'Mute audio source'}
        </Button>
        <p className="text-xs text-muted-foreground">
          Mute silences the shared audio without changing your selected source.
        </p>
        <div className="space-y-1.5">
          {renderBtn({ representative: NO_AUDIO_APP, members: [NO_AUDIO_APP] })}
          {renderBtn(DESKTOP_AUDIO_GROUP)}
          {groups.length > 0 && <div className="border-t border-border my-1.5" />}
          {groups.map((group) => renderBtn(group))}
        </div>
      </section>
    );
  },
);

AudioAppPicker.displayName = 'AudioAppPicker';
