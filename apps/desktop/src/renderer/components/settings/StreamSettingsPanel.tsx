import type { ResolutionPreset, VideoCodec } from '@slopcast/shared-types';
import { codecLabel } from '@slopcast/shared-types';
import { ChevronDown } from 'lucide-react';
import type React from 'react';
import { memo } from 'react';
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card';
import { Input } from '@/components/ui/input';
import type { CodecInfo } from '@/utils/codecs';

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
                <select
                  id="select-video-codec"
                  value={videoCodec}
                  onChange={(e) => setVideoCodec(e.target.value as VideoCodec)}
                  className="w-full bg-secondary border border-border rounded-lg px-3 py-2 text-sm text-foreground focus:outline-none focus:ring-2 focus:ring-safelight cursor-pointer"
                >
                  {availableCodecs.map((info) => (
                    <option key={info.codec} value={info.codec} className="bg-popover text-popover-foreground">
                      {codecLabel(info.codec)} {codecOptionSuffix(info)}
                    </option>
                  ))}
                </select>
              </div>

              <div className="space-y-1.5">
                <label
                  htmlFor="select-resolution"
                  className="text-xs font-semibold uppercase tracking-wider text-muted-foreground block"
                >
                  Resolution
                </label>
                <select
                  id="select-resolution"
                  value={resolution}
                  onChange={(e) => setResolution(e.target.value as ResolutionPreset)}
                  className="w-full bg-secondary border border-border rounded-lg px-3 py-2 text-sm text-foreground focus:outline-none focus:ring-2 focus:ring-safelight cursor-pointer"
                >
                  <option value="480p" className="bg-popover text-popover-foreground">
                    480p (SD)
                  </option>
                  <option value="720p" className="bg-popover text-popover-foreground">
                    720p (HD)
                  </option>
                  <option value="1080p" className="bg-popover text-popover-foreground">
                    1080p (Full HD)
                  </option>
                  <option value="1440p" className="bg-popover text-popover-foreground">
                    1440p (QHD)
                  </option>
                  <option value="2160p" className="bg-popover text-popover-foreground">
                    4K (Ultra HD)
                  </option>
                </select>
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
                <select
                  id="select-fps"
                  value={streamFps}
                  onChange={(e) => setStreamFps(Number(e.target.value))}
                  className="w-full bg-secondary border border-border rounded-lg px-3 py-2 text-sm text-foreground focus:outline-none focus:ring-2 focus:ring-safelight cursor-pointer"
                >
                  <option value={15} className="bg-popover text-popover-foreground">
                    15 fps
                  </option>
                  <option value={24} className="bg-popover text-popover-foreground">
                    24 fps
                  </option>
                  <option value={30} className="bg-popover text-popover-foreground">
                    30 fps
                  </option>
                  <option value={60} className="bg-popover text-popover-foreground">
                    60 fps
                  </option>
                </select>
              </div>

              <div className="space-y-1.5">
                <label
                  htmlFor="select-bitrate"
                  className="text-xs font-semibold uppercase tracking-wider text-muted-foreground block"
                >
                  Bitrate Limit
                </label>
                <select
                  id="select-bitrate"
                  value={bitrateLimit}
                  onChange={(e) => setBitrateLimit(Number(e.target.value))}
                  className="w-full bg-secondary border border-border rounded-lg px-3 py-2 text-sm text-foreground focus:outline-none focus:ring-2 focus:ring-safelight cursor-pointer"
                >
                  <option value={1_000_000} className="bg-popover text-popover-foreground">
                    1 Mbps
                  </option>
                  <option value={2_000_000} className="bg-popover text-popover-foreground">
                    2 Mbps
                  </option>
                  <option value={4_000_000} className="bg-popover text-popover-foreground">
                    4 Mbps
                  </option>
                  <option value={6_000_000} className="bg-popover text-popover-foreground">
                    6 Mbps
                  </option>
                  <option value={10_000_000} className="bg-popover text-popover-foreground">
                    10 Mbps
                  </option>
                  <option value={20_000_000} className="bg-popover text-popover-foreground">
                    20 Mbps
                  </option>
                  <option value={30_000_000} className="bg-popover text-popover-foreground">
                    30 Mbps
                  </option>
                  <option value={50_000_000} className="bg-popover text-popover-foreground">
                    50 Mbps
                  </option>
                  <option value={80_000_000} className="bg-popover text-popover-foreground">
                    80 Mbps
                  </option>
                </select>
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
