import type { ResolutionPreset, VideoCodec } from '@slopcast/shared-types';
import { codecLabel } from '@slopcast/shared-types';
import { ChevronDown } from 'lucide-react';
import type React from 'react';
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card';

export interface CodecInfo {
  codec: VideoCodec;
  label: string;
  hardware: boolean;
  recommended: boolean;
}

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

export const StreamSettingsPanel: React.FC<StreamSettingsPanelProps> = ({
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
  const containerClass = streamSettingsOpen ? 'max-h-[600px] opacity-100 mt-4' : 'max-h-0 opacity-0 overflow-hidden';

  return (
    <Card className="border-border/60 bg-card/60 backdrop-blur-sm shadow-xl">
      <CardHeader
        className="pb-3 cursor-pointer select-none"
        onClick={() => setStreamSettingsOpen(!streamSettingsOpen)}
      >
        <CardTitle className="text-sm font-semibold flex items-center justify-between">
          <span>Stream & Encoder Settings</span>
          <ChevronDown
            className={`w-4 h-4 text-muted-foreground transition-transform duration-200 ${openClass}`}
            aria-hidden="true"
          />
        </CardTitle>
      </CardHeader>
      <div className={`transition-all duration-300 ease-in-out ${containerClass}`}>
        <CardContent className="space-y-4 pt-1">
          <div className="grid grid-cols-1 sm:grid-cols-2 gap-4">
            <div className="space-y-1.5">
              <label htmlFor="select-video-codec" className="text-xs font-medium text-foreground block">
                Video Codec
              </label>
              <select
                id="select-video-codec"
                value={videoCodec}
                onChange={(e) => setVideoCodec(e.target.value as VideoCodec)}
                className="w-full bg-secondary border border-border rounded-lg px-3 py-2 text-xs font-mono focus:outline-none focus:ring-2 focus:ring-safelight"
              >
                {availableCodecs.map((info) => (
                  <option key={info.codec} value={info.codec}>
                    {codecLabel(info.codec)} {codecOptionSuffix(info)}
                  </option>
                ))}
              </select>
            </div>

            <div className="space-y-1.5">
              <label htmlFor="select-resolution" className="text-xs font-medium text-foreground block">
                Resolution
              </label>
              <select
                id="select-resolution"
                value={resolution}
                onChange={(e) => setResolution(e.target.value as ResolutionPreset)}
                className="w-full bg-secondary border border-border rounded-lg px-3 py-2 text-xs font-mono focus:outline-none focus:ring-2 focus:ring-safelight cursor-pointer"
              >
                <option value="1080p">1080p (Full HD)</option>
                <option value="1440p">1440p (QHD)</option>
                <option value="2160p">4K (Ultra HD)</option>
              </select>
            </div>
          </div>

          <div className="grid grid-cols-1 sm:grid-cols-2 gap-4">
            <div className="space-y-1.5">
              <label htmlFor="select-fps" className="text-xs font-medium text-foreground block">
                Frame Rate
              </label>
              <select
                id="select-fps"
                value={streamFps}
                onChange={(e) => setStreamFps(Number(e.target.value))}
                className="w-full bg-secondary border border-border rounded-lg px-3 py-2 text-xs font-mono focus:outline-none focus:ring-2 focus:ring-safelight cursor-pointer"
              >
                <option value={15}>15 fps</option>
                <option value={24}>24 fps</option>
                <option value={30}>30 fps</option>
                <option value={60}>60 fps</option>
              </select>
            </div>

            <div className="space-y-1.5">
              <label htmlFor="select-bitrate" className="text-xs font-medium text-foreground block">
                Bitrate Limit
              </label>
              <select
                id="select-bitrate"
                value={bitrateLimit}
                onChange={(e) => setBitrateLimit(Number(e.target.value))}
                className="w-full bg-secondary border border-border rounded-lg px-3 py-2 text-xs font-mono focus:outline-none focus:ring-2 focus:ring-safelight cursor-pointer"
              >
                <option value={1_000_000}>1 Mbps</option>
                <option value={2_000_000}>2 Mbps</option>
                <option value={4_000_000}>4 Mbps</option>
                <option value={6_000_000}>6 Mbps</option>
                <option value={10_000_000}>10 Mbps</option>
                <option value={20_000_000}>20 Mbps</option>
                <option value={30_000_000}>30 Mbps</option>
                <option value={50_000_000}>50 Mbps</option>
                <option value={80_000_000}>80 Mbps</option>
              </select>
            </div>
          </div>

          {setApiEndpoint && (
            <div className="space-y-1.5">
              <label htmlFor="api-endpoint" className="text-xs font-medium text-foreground block">
                API Endpoint
              </label>
              <input
                id="api-endpoint"
                type="url"
                value={apiEndpoint ?? 'http://localhost:3001'}
                onChange={(e) => setApiEndpoint(e.target.value)}
                placeholder="http://localhost:3001"
                className="w-full rounded-lg bg-secondary border border-border text-xs text-foreground py-2 px-3 focus:outline-none focus:ring-2 focus:ring-safelight font-mono"
              />
            </div>
          )}
        </CardContent>
      </div>
    </Card>
  );
};
