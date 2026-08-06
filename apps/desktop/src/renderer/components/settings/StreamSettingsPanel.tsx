import type { ResolutionPreset, VideoCodec } from '@slopcast/shared-types';
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
import type { CodecInfo } from '@/utils/codecs';
import { groupCodecsByHardware } from '@/utils/codecs';

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
    apiEndpoint,
    setApiEndpoint,
  }) => {
    const openClass = streamSettingsOpen ? 'rotate-0' : '-rotate-90';
    const containerClass = streamSettingsOpen ? 'max-h-[600px] opacity-100' : 'max-h-0 opacity-0 overflow-hidden';
    const { hardware: hardwareCodecs, software: softwareCodecs } = groupCodecsByHardware(availableCodecs);

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

              <div className="space-y-1.5">
                <label
                  htmlFor="select-bitrate"
                  className="text-xs font-semibold uppercase tracking-wider text-muted-foreground block"
                >
                  Bitrate Limit
                </label>
                <Select value={String(bitrateLimit)} onValueChange={(v) => setBitrateLimit(Number(v))}>
                  <SelectTrigger id="select-bitrate">
                    <SelectValue />
                  </SelectTrigger>
                  <SelectContent>
                    <SelectItem value="1000000">1 Mbps</SelectItem>
                    <SelectItem value="2000000">2 Mbps</SelectItem>
                    <SelectItem value="4000000">4 Mbps</SelectItem>
                    <SelectItem value="6000000">6 Mbps</SelectItem>
                    <SelectItem value="10000000">10 Mbps</SelectItem>
                    <SelectItem value="20000000">20 Mbps</SelectItem>
                    <SelectItem value="30000000">30 Mbps</SelectItem>
                    <SelectItem value="50000000">50 Mbps</SelectItem>
                    <SelectItem value="80000000">80 Mbps</SelectItem>
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
