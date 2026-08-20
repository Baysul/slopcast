import type { MotionMode, ResolutionPreset, VideoCodec } from '@slopcast/shared-types';
import { ChevronDown } from 'lucide-react';
import type React from 'react';
import { memo } from 'react';
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card';
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
import { manualBitrateOptions, recommendedBitrateRange } from '@/utils/bitrate';
import type { CodecInfo } from '@/utils/codecs';
import { groupCodecsByHardware } from '@/utils/codecs';

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

export interface StreamSettingsPanelProps {
  streamSettingsOpen: boolean;
  setStreamSettingsOpen: (open: boolean) => void;
  videoCodec: VideoCodec;
  setVideoCodec: (codec: VideoCodec) => void;
  availableCodecs: CodecInfo[];
  codecOptionSuffix: (info: CodecInfo) => string;
  resolution: ResolutionPreset;
  setResolution: (res: ResolutionPreset) => void;
  streamFps: number;
  setStreamFps: (fps: number) => void;
  bitrateLimit: number;
  setBitrateLimit: (bitrate: number) => void;
  effectiveBitrate: number;
  autoBitrate: boolean;
  setAutoBitrate: (auto: boolean) => void;
  motionMode: MotionMode;
  setMotionMode: (mode: MotionMode) => void;
  apiEndpoint?: string;
  setApiEndpoint?: (endpoint: string) => void;
}

export const StreamSettingsPanel: React.FC<StreamSettingsPanelProps> = memo(
  ({
    streamSettingsOpen,
    setStreamSettingsOpen,
    videoCodec,
    setVideoCodec,
    availableCodecs,
    codecOptionSuffix,
    resolution,
    setResolution,
    streamFps,
    setStreamFps,
    bitrateLimit,
    setBitrateLimit,
    effectiveBitrate,
    autoBitrate,
    setAutoBitrate,
    motionMode,
    setMotionMode,
    apiEndpoint,
    setApiEndpoint,
  }) => {
    const openClass = streamSettingsOpen ? 'rotate-0' : '-rotate-90';
    const containerClass = streamSettingsOpen ? 'max-h-[800px] opacity-100' : 'max-h-0 opacity-0 overflow-hidden';
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
      <Card className="border-border/60 bg-card/60 backdrop-blur-sm shadow-xl">
        <CardHeader className="p-0">
          <button
            type="button"
            aria-expanded={streamSettingsOpen}
            aria-controls="stream-settings-content"
            onClick={() => setStreamSettingsOpen(!streamSettingsOpen)}
            className={`w-full text-left px-6 py-5 focus:outline-none focus-visible:ring-2 focus-visible:ring-safelight/70 focus-visible:ring-offset-2 focus-visible:ring-offset-background rounded-lg transition-colors hover:bg-accent/10 ${
              streamSettingsOpen ? 'rounded-b-none pb-3' : ''
            }`}
          >
            <CardTitle className="text-sm font-semibold flex items-center justify-between text-foreground">
              <span>Stream & Encoder Settings</span>
              <ChevronDown
                className={`w-4 h-4 text-muted-foreground transition-transform duration-200 ${openClass}`}
                aria-hidden="true"
              />
            </CardTitle>
          </button>
        </CardHeader>
        <div id="stream-settings-content" className={`transition-all duration-300 ease-in-out ${containerClass}`}>
          <CardContent className="space-y-4 px-6 pb-6 pt-1">
            <div className="grid grid-cols-1 sm:grid-cols-2 gap-4">
              <div className="space-y-1.5">
                <label
                  htmlFor="select-video-codec"
                  className="text-xs font-semibold uppercase tracking-wider text-muted-foreground block"
                >
                  Video Codec
                </label>
                <Select value={videoCodec} onValueChange={(v) => setVideoCodec(v as VideoCodec)}>
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
                <Select value={resolution} onValueChange={(v) => setResolution(v as ResolutionPreset)}>
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
                  onValueChange={(v) => setMotionMode(v as MotionMode)}
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

            {setApiEndpoint && (
              <div className="space-y-1.5">
                <label
                  htmlFor="api-endpoint"
                  className="text-xs font-semibold uppercase tracking-wider text-muted-foreground block"
                >
                  API Endpoint
                </label>
                <Input
                  id="api-endpoint"
                  type="url"
                  value={apiEndpoint ?? 'http://localhost:3001'}
                  onChange={(e) => setApiEndpoint(e.target.value)}
                  placeholder="http://localhost:3001"
                  className="w-full bg-secondary text-sm text-foreground"
                />
              </div>
            )}
          </CardContent>
        </div>
      </Card>
    );
  },
);
