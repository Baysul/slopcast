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
                Target Resolution
              </label>
              <select
                id="select-resolution"
                value={resolution}
                onChange={(e) => setResolution(e.target.value as ResolutionPreset)}
                className="w-full bg-secondary border border-border rounded-lg px-3 py-2 text-xs font-mono focus:outline-none focus:ring-2 focus:ring-safelight"
              >
                <option value="1080p">1080p (1920×1080)</option>
                <option value="720p">720p (1280×720)</option>
                <option value="480p">480p (854×480)</option>
              </select>
            </div>

            <div className="space-y-1.5">
              <label htmlFor="select-fps" className="text-xs font-medium text-foreground block">
                Target Frame Rate
              </label>
              <select
                id="select-fps"
                value={streamFps}
                onChange={(e) => setStreamFps(Number(e.target.value))}
                className="w-full bg-secondary border border-border rounded-lg px-3 py-2 text-xs font-mono focus:outline-none focus:ring-2 focus:ring-safelight"
              >
                <option value={60}>60 FPS</option>
                <option value={30}>30 FPS</option>
                <option value={15}>15 FPS</option>
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
                className="w-full bg-secondary border border-border rounded-lg px-3 py-2 text-xs font-mono focus:outline-none focus:ring-2 focus:ring-safelight"
              >
                <option value={8_000_000}>8.0 Mbps</option>
                <option value={6_000_000}>6.0 Mbps</option>
                <option value={4_000_000}>4.0 Mbps</option>
                <option value={2_500_000}>2.5 Mbps</option>
              </select>
            </div>
          </div>
        </CardContent>
      </div>
    </Card>
  );
};
