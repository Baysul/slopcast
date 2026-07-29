import type { AudioApp } from '@slopcast/shared-types';
import { Check } from 'lucide-react';
import type React from 'react';
import type { AudioAppGroup } from '../../utils/audio-grouping';
import { AudioLevelMeter } from '../ui/AudioLevelMeter';

export interface AudioPickerPanelProps {
  audioAppGroups: AudioAppGroup[];
  selectedAudioAppId: number | null;
  autoDetectedApp: AudioApp | null;
  audioLevels: ReadonlyMap<number, number>;
  loadAudioApps: () => void;
  setSelectedAudioAppId: (id: number | null) => void;
  setAudioAppExplicitlySet: (v: boolean) => void;
  setAutoDetectedApp: (app: AudioApp | null) => void;
}

const displayName = (app: AudioApp): string => app.name;

const groupSubLabel = (group: AudioAppGroup, isDesktopAudio: boolean): string | null => {
  if (isDesktopAudio) return 'All system audio';
  const { members } = group;
  let label: string | undefined;
  for (const m of members) {
    if (m.mediaTitle) {
      label = m.mediaTitle;
      break;
    }
  }
  if (!label) {
    for (const m of members) {
      if (m.windowTitle) {
        label = m.windowTitle;
        break;
      }
    }
  }
  if (label) {
    if (members.length > 1) return `${label} \u00B7 ${members.length} streams`;
    return label;
  }
  if (members.length > 1) return `${members.length} audio streams`;
  return null;
};

const pickerRowClass = (isSelected: boolean, isDesktopAudio: boolean): string => {
  if (isSelected && isDesktopAudio) {
    return 'bg-amber-950/40 border-amber-500/40 text-amber-200';
  }
  if (isSelected) {
    return 'bg-emerald-950/40 border-emerald-500/40 text-emerald-200';
  }
  return 'bg-background/60 border-gray-800/60 text-gray-400 hover:border-gray-700 hover:text-gray-300';
};

export const AudioPickerPanel: React.FC<AudioPickerPanelProps> = ({
  audioAppGroups,
  selectedAudioAppId,
  autoDetectedApp,
  audioLevels,
  loadAudioApps,
  setSelectedAudioAppId,
  setAudioAppExplicitlySet,
  setAutoDetectedApp,
}) => {
  const renderBtn = (group: AudioAppGroup, isDesktopAudio: boolean) => {
    const { representative, members } = group;
    const isSelected = members.some((m) => m.id === selectedAudioAppId);
    const isAutoDetected = members.some((m) => m.id === autoDetectedApp?.id);
    const level = members.reduce((max: number, m: AudioApp) => Math.max(max, audioLevels.get(m.id) ?? 0), 0);
    const btnClass = `flex items-center justify-between p-2.5 rounded-lg border text-xs transition-all cursor-pointer text-left w-full focus:outline-none focus-visible:ring-2 focus-visible:ring-safelight/70 focus-visible:ring-offset-2 focus-visible:ring-offset-background ${pickerRowClass(
      isSelected,
      isDesktopAudio,
    )}`;
    return (
      <button
        key={representative.id}
        type="button"
        onClick={() => {
          if (isSelected) {
            setAudioAppExplicitlySet(false);
            setSelectedAudioAppId(null);
            setAutoDetectedApp(null);
          } else {
            setAudioAppExplicitlySet(true);
            setSelectedAudioAppId(representative.id);
            setAutoDetectedApp(null);
          }
        }}
        className={btnClass}
      >
        <div className="min-w-0 flex-1">
          <div className="flex items-center gap-2">
            <span className="font-semibold truncate min-w-0">{displayName(representative)}</span>
            {!isDesktopAudio && <AudioLevelMeter level={level} />}
          </div>
          {(() => {
            const label = groupSubLabel(group, isDesktopAudio);
            if (!label) return null;
            return (
              <div className="flex items-center gap-2 mt-0.5">
                <span className="text-[10px] opacity-60">{label}</span>
                {isAutoDetected && (
                  <span className="text-[10px] bg-safelight-glow text-safelight/80 px-1.5 py-0.5 rounded-full">
                    auto
                  </span>
                )}
              </div>
            );
          })()}
        </div>
        {isSelected && (
          <Check
            className={`w-4 h-4 shrink-0 ${isDesktopAudio ? 'text-amber-300' : 'text-emerald-300'}`}
            aria-hidden="true"
          />
        )}
      </button>
    );
  };

  const desktopAudio: AudioApp = {
    id: -1,
    name: 'Desktop Audio (All System Sound)',
    processId: 0,
    clientId: null,
    mediaTitle: null,
  };
  const items: React.ReactNode[] = [renderBtn({ representative: desktopAudio, members: [desktopAudio] }, true)];
  if (audioAppGroups.length > 0) {
    items.push(<div key="divider" className="border-t border-gray-800 my-1.5" />);
    for (const group of audioAppGroups) {
      items.push(renderBtn(group, false));
    }
  }

  return (
    <div className="bg-gray-900/80 border border-gray-800 rounded-xl p-6 space-y-4">
      <div className="flex items-center justify-between">
        <h2 className="text-xs font-semibold uppercase tracking-wider text-gray-400 flex items-center gap-2">
          Window Audio Capture
          {autoDetectedApp && (
            <span className="text-[10px] font-normal text-safelight bg-safelight-glow px-2 py-0.5 rounded-full border border-safelight/30">
              Auto \u2713
            </span>
          )}
        </h2>
        <button
          type="button"
          onClick={loadAudioApps}
          className="text-xs text-gray-500 hover:text-gray-300 transition-colors focus:outline-none focus-visible:ring-2 focus-visible:ring-safelight/70 focus-visible:ring-offset-2 focus-visible:ring-offset-background rounded-md"
        >
          Refresh
        </button>
      </div>

      <p className="text-xs text-gray-500 leading-relaxed">
        Auto-detected from your window selection. Click an app below to override \u2014 only that app's audio is
        streamed. Select <strong className="text-gray-300">Desktop Audio</strong> to capture all system sound.
      </p>

      <div className="space-y-1.5 max-h-56 overflow-y-auto pr-1">{items}</div>
    </div>
  );
};
