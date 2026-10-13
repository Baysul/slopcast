import type { MotionMode, ResolutionPreset, VideoCodec } from '@slopcast/shared-types';
import type React from 'react';
import { memo, useCallback, useEffect, useState } from 'react';
import { desktopApi } from '@/api/desktop';
import { Input } from '@/components/ui/input';
import {
  Select,
  SelectContent,
  SelectGroup,
  SelectItem,
  SelectLabel,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select';
import { Slider } from '@/components/ui/slider';
import { Switch } from '@/components/ui/switch';
import { useStreamSettings } from '@/hooks/useStreamSettings';
import { notify } from '@/lib/toast';
import type { ControlState } from '@/types/preferences';
import { manualBitrateOptions, recommendBitrateCap, recommendedBitrateRange } from '@/utils/bitrate';
import { codecOptionSuffix, groupCodecsByHardware } from '@/utils/codecs';
import { ApiEndpointField } from './ApiEndpointField';

const BITS_PER_MEGABIT = 1_000_000;

const formatMegabits = (bitrate: number): string => (bitrate / BITS_PER_MEGABIT).toFixed(1).replace(/\.0$/, '');

interface BitrateGuidanceInput {
  autoBitrate: boolean;
  bitrateLimit: number;
  recommendedMinimum: number;
  recommendedMaximum: number;
  recommendedRangeLabel: string;
  streamProfile: string;
}

interface BitrateGuidance {
  message: string;
  needsAttention: boolean;
}

const getBitrateGuidance = ({
  autoBitrate,
  bitrateLimit,
  recommendedMinimum,
  recommendedMaximum,
  recommendedRangeLabel,
  streamProfile,
}: BitrateGuidanceInput): BitrateGuidance => {
  if (autoBitrate) {
    return {
      message: `Automatic bitrate targets ${recommendedRangeLabel} for ${streamProfile}.`,
      needsAttention: false,
    };
  }
  if (bitrateLimit < recommendedMinimum) {
    return {
      message: `Below the recommended ${recommendedRangeLabel} range for ${streamProfile} — quality may suffer.`,
      needsAttention: true,
    };
  }
  if (bitrateLimit > recommendedMaximum) {
    return {
      message: `Above the recommended ${recommendedRangeLabel} range for ${streamProfile} — this may use more bandwidth without improving quality.`,
      needsAttention: true,
    };
  }

  return {
    message: `Within the recommended ${recommendedRangeLabel} range for ${streamProfile}.`,
    needsAttention: false,
  };
};

export const StreamSettingsPanel: React.FC = memo(() => {
  const {
    videoCodec,
    setVideoCodec,
    availableCodecs,
    resolution,
    setResolution,
    streamFps,
    setStreamFps,
    bitrateLimit,
    setBitrateLimit,
    autoBitrate,
    setAutoBitrate,
    motionMode,
    setMotionMode,
    apiEndpoint,
    pendingApiEndpoint,
    settingsHydrated,
  } = useStreamSettings();
  const [controls, setControls] = useState<ControlState | null>(null);
  const [contextError, setContextError] = useState('');
  const onApiEndpointValidated = useCallback((endpoint: string) => {
    void desktopApi
      .editApiEndpoint({ kind: 'validate', endpoint })
      .catch((cause: unknown) => notify('error', 'API endpoint could not update', String(cause)));
  }, []);
  const onApiEndpointReset = useCallback(() => {
    void desktopApi
      .editApiEndpoint({ kind: 'reset' })
      .catch((cause: unknown) => notify('error', 'API endpoint could not reset', String(cause)));
  }, []);

  useEffect(() => {
    let disposed = false;
    let received = false;
    const unlisten = desktopApi.onControlStateChanged((state) => {
      received = true;
      if (disposed) return;
      setControls(state);
      setContextError('');
    });
    void unlisten
      .then(async () => {
        const state = await desktopApi.getControlState();
        if (!disposed && !received) setControls(state);
      })
      .catch((cause: unknown) => {
        if (!disposed) setContextError(String(cause));
      });
    return () => {
      disposed = true;
      void unlisten.then((stop) => stop());
    };
  }, []);

  if (!settingsHydrated)
    return (
      <p className="text-sm text-muted-foreground" role="status">
        Loading stream settings…
      </p>
    );

  let effectiveBitrate = bitrateLimit;
  if (autoBitrate)
    effectiveBitrate = recommendBitrateCap({ codec: videoCodec, resolution, fps: streamFps, motionTier: 'static' });
  const roomEndpoint = controls?.roomEndpoint ?? null;
  const endpointValidationRevision = controls?.endpointValidationRevision ?? 0;
  const endpointControlsDisabled = !controls || controls.endpointControlsDisabled;
  const { hardware: hardwareCodecs, software: softwareCodecs } = groupCodecsByHardware(availableCodecs);
  const bitrateOptions = manualBitrateOptions(videoCodec);
  const bitrateMaximum = Math.max(...bitrateOptions);
  const [recommendedMinimum, recommendedMaximum] = recommendedBitrateRange({
    codec: videoCodec,
    resolution,
    fps: streamFps,
    motionTier: 'static',
  });
  const recommendedRangeLabel = `${formatMegabits(recommendedMinimum)}–${formatMegabits(recommendedMaximum)} Mbps`;
  const bitrateGuidance = getBitrateGuidance({
    autoBitrate,
    bitrateLimit,
    recommendedMinimum,
    recommendedMaximum,
    recommendedRangeLabel,
    streamProfile: `${resolution}${streamFps}`,
  });

  return (
    <div className="space-y-6">
      <div className="space-y-2">
        <h2 className="font-semibold">Stream & encoder</h2>
        <p className="text-sm text-muted-foreground leading-relaxed">Choose video quality and your API server.</p>
      </div>
      <div id="stream-settings-content" className="space-y-4">
        <div className="grid grid-cols-1 sm:grid-cols-2 gap-4">
          <div className="space-y-1.5">
            <label
              htmlFor="select-video-codec"
              className="text-xs font-semibold uppercase tracking-wider text-muted-foreground block"
            >
              Video Codec
            </label>
            <Select
              value={videoCodec}
              onValueChange={(value) => {
                // SAFETY: every item value comes from a CodecInfo codec.
                setVideoCodec(value as VideoCodec);
              }}
            >
              <SelectTrigger id="select-video-codec">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                {hardwareCodecs.length > 0 && (
                  <SelectGroup>
                    <SelectLabel>Hardware</SelectLabel>
                    {hardwareCodecs.map((info) => (
                      <SelectItem key={info.codec} value={info.codec}>
                        {info.label}
                        {codecOptionSuffix(info)}
                      </SelectItem>
                    ))}
                  </SelectGroup>
                )}
                {softwareCodecs.length > 0 && (
                  <SelectGroup>
                    {hardwareCodecs.length > 0 && <SelectLabel>Software</SelectLabel>}
                    {softwareCodecs.map((info) => (
                      <SelectItem key={info.codec} value={info.codec}>
                        {info.label}
                        {codecOptionSuffix(info)}
                      </SelectItem>
                    ))}
                  </SelectGroup>
                )}
              </SelectContent>
            </Select>
          </div>

          <div className="space-y-1.5">
            <label
              htmlFor="select-resolution"
              className="text-xs font-semibold uppercase tracking-wider text-muted-foreground block"
            >
              Resolution
            </label>
            <Select
              value={resolution}
              onValueChange={(value) => {
                // SAFETY: the items below exhaust the ResolutionPreset values.
                setResolution(value as ResolutionPreset);
              }}
            >
              <SelectTrigger id="select-resolution">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                <SelectItem value="480p">480p (SD)</SelectItem>
                <SelectItem value="720p">720p (HD)</SelectItem>
                <SelectItem value="1080p">1080p (Full HD)</SelectItem>
                <SelectItem value="1440p">1440p (QHD)</SelectItem>
                <SelectItem value="2160p">4K (Ultra HD)</SelectItem>
              </SelectContent>
            </Select>
          </div>
        </div>

        <div className="grid grid-cols-1 sm:grid-cols-2 gap-4">
          <div className="space-y-1.5">
            <label
              htmlFor="select-fps"
              className="text-xs font-semibold uppercase tracking-wider text-muted-foreground block"
            >
              Frame Rate
            </label>
            <Select value={String(streamFps)} onValueChange={(v) => setStreamFps(Number(v))}>
              <SelectTrigger id="select-fps">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                <SelectItem value="15">15 fps</SelectItem>
                <SelectItem value="24">24 fps</SelectItem>
                <SelectItem value="30">30 fps</SelectItem>
                <SelectItem value="60">60 fps</SelectItem>
              </SelectContent>
            </Select>
          </div>

          <div className="space-y-2">
            <label
              htmlFor="input-bitrate"
              className="text-xs font-semibold uppercase tracking-wider text-muted-foreground block"
            >
              Bitrate Limit
            </label>
            <div className="flex items-center gap-3">
              <Input
                id="input-bitrate"
                type="number"
                min={1}
                max={bitrateMaximum / BITS_PER_MEGABIT}
                step={0.5}
                value={formatMegabits(autoBitrate ? effectiveBitrate : bitrateLimit)}
                onChange={(event) => {
                  const megabits = Number(event.currentTarget.value);
                  if (!Number.isFinite(megabits)) return;

                  const boundedMegabits = Math.min(
                    bitrateMaximum / BITS_PER_MEGABIT,
                    Math.max(1, Math.round(megabits * 2) / 2),
                  );
                  setBitrateLimit(boundedMegabits * BITS_PER_MEGABIT);
                }}
                disabled={autoBitrate}
                className="w-20 shrink-0 bg-secondary text-center font-mono tabular-nums text-foreground"
              />
              <span className="shrink-0 text-sm text-muted-foreground">Mbps</span>
              <Slider
                aria-label="Bitrate limit"
                aria-describedby="bitrate-guidance"
                min={BITS_PER_MEGABIT}
                max={bitrateMaximum}
                step={500_000}
                value={[autoBitrate ? effectiveBitrate : bitrateLimit]}
                onValueChange={(values) => {
                  const nextBitrate = values[0];
                  if (nextBitrate == null) return;

                  setBitrateLimit(nextBitrate);
                }}
                disabled={autoBitrate}
              />
            </div>
            <p
              id="bitrate-guidance"
              role="status"
              aria-live="polite"
              className={`text-xs leading-relaxed ${bitrateGuidance.needsAttention ? 'text-safelight' : 'text-muted-foreground'}`}
            >
              {bitrateGuidance.message}
            </p>
          </div>
        </div>

        <div className="space-y-3 pt-1">
          <div className="flex items-center justify-between">
            <div className="space-y-0.5">
              <label
                htmlFor="auto-bitrate"
                className="text-xs font-semibold uppercase tracking-wider text-muted-foreground"
              >
                Automatic Bitrate
              </label>
              <p className="text-xs text-muted-foreground">
                Derive the bitrate from codec, resolution, framerate and content motion.
              </p>
            </div>
            <Switch id="auto-bitrate" checked={autoBitrate} onCheckedChange={setAutoBitrate} />
          </div>

          <div className="space-y-1.5">
            <label
              htmlFor="select-motion"
              className="text-xs font-semibold uppercase tracking-wider text-muted-foreground block"
            >
              Content Motion
            </label>
            <Select
              value={motionMode}
              onValueChange={(value) => {
                // SAFETY: the items below exhaust the MotionMode values.
                setMotionMode(value as MotionMode);
              }}
              disabled={!autoBitrate}
            >
              <SelectTrigger id="select-motion">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                <SelectItem value="auto">Auto-detect</SelectItem>
                <SelectItem value="static">Static (documents, slides)</SelectItem>
                <SelectItem value="mixed">Mixed (browsing, demos)</SelectItem>
                <SelectItem value="dynamic">Dynamic (gaming, video)</SelectItem>
              </SelectContent>
            </Select>
          </div>
        </div>

        {contextError && (
          <p className="text-sm text-destructive" role="alert">
            Room controls unavailable: {contextError}
          </p>
        )}
        <ApiEndpointField
          activeEndpoint={apiEndpoint}
          pendingEndpoint={pendingApiEndpoint}
          roomEndpoint={roomEndpoint}
          validationRevision={endpointValidationRevision}
          disabled={endpointControlsDisabled}
          onValidated={onApiEndpointValidated}
          onReset={onApiEndpointReset}
        />
      </div>
    </div>
  );
});
