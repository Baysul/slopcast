import type { ResolutionPreset, VideoCodec } from '@slopcast/shared-types';
import { ChevronDown } from 'lucide-react';
import type React from 'react';

export interface StreamSettingsPanelProps {
  streamSettingsOpen: boolean;
  streamFps: number;
  bitrateLimit: number;
  videoCodec: VideoCodec;
  resolution: ResolutionPreset;
  apiEndpoint: string;
  setStreamSettingsOpen: (open: boolean) => void;
  setStreamFps: (fps: number) => void;
  setBitrateLimit: (bps: number) => void;
  setVideoCodec: (codec: VideoCodec) => void;
  setResolution: (res: ResolutionPreset) => void;
  setApiEndpoint: (url: string) => void;
}

const CODEC_OPTIONS: { value: VideoCodec; label: string }[] = [
  { value: 'av1', label: 'AV1' },
  { value: 'vp9', label: 'VP9' },
  { value: 'h264', label: 'H.264' },
  { value: 'vp8', label: 'VP8' },
];

export const StreamSettingsPanel: React.FC<StreamSettingsPanelProps> = ({
  streamSettingsOpen,
  streamFps,
  bitrateLimit,
  videoCodec,
  resolution,
  apiEndpoint,
  setStreamSettingsOpen,
  setStreamFps,
  setBitrateLimit,
  setVideoCodec,
  setResolution,
  setApiEndpoint,
}) => (
  <div className="bg-gray-900/80 border border-gray-800 rounded-xl">
    <button
      type="button"
      onClick={() => setStreamSettingsOpen(!streamSettingsOpen)}
      className={`flex w-full items-center justify-between gap-3 px-6 pt-6 text-left focus:outline-none focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-safelight/70 ${
        streamSettingsOpen ? 'pb-0' : 'pb-6'
      }`}
      aria-expanded={streamSettingsOpen}
    >
      <div>
        <h2 className="text-xs font-semibold uppercase tracking-wider text-gray-400">Stream Settings</h2>
        <p className="text-xs text-gray-500 leading-relaxed mt-1">
          FPS, bitrate, codec, and resolution apply in real time via the native encoding pipeline.
        </p>
      </div>
      <ChevronDown
        className={`size-4 shrink-0 text-gray-500 transition-transform duration-200 ${
          streamSettingsOpen ? 'rotate-0' : '-rotate-90'
        }`}
      />
    </button>
    <div
      className={`overflow-hidden transition-all duration-200 ease-out ${
        streamSettingsOpen ? 'max-h-[600px] opacity-100' : 'max-h-0 opacity-0'
      }`}
    >
      <div className="space-y-5 p-6">
        <div className="grid grid-cols-1 sm:grid-cols-2 gap-4">
          <div className="space-y-1.5">
            <label
              htmlFor="stream-codec"
              className="block text-[10px] font-semibold uppercase tracking-[0.05em] text-gray-500"
            >
              Video Codec
            </label>
            <select
              id="stream-codec"
              value={videoCodec}
              onChange={(e) => setVideoCodec(e.target.value as VideoCodec)}
              className="w-full rounded-lg bg-background/90 border border-gray-800 text-sm text-gray-200 py-2 px-3 focus:outline-none focus-visible:ring-2 focus-visible:ring-safelight/70 focus-visible:ring-offset-2 focus-visible:ring-offset-background cursor-pointer"
            >
              {CODEC_OPTIONS.map((opt) => (
                <option key={opt.value} value={opt.value}>
                  {opt.label}
                </option>
              ))}
            </select>
          </div>

          <div className="space-y-1.5">
            <label
              htmlFor="stream-resolution"
              className="block text-[10px] font-semibold uppercase tracking-[0.05em] text-gray-500"
            >
              Resolution
            </label>
            <select
              id="stream-resolution"
              value={resolution}
              onChange={(e) => setResolution(e.target.value as ResolutionPreset)}
              className="w-full rounded-lg bg-background/90 border border-gray-800 text-sm text-gray-200 py-2 px-3 focus:outline-none focus-visible:ring-2 focus-visible:ring-safelight/70 focus-visible:ring-offset-2 focus-visible:ring-offset-background cursor-pointer"
            >
              <option value="1080p">1080p (Full HD)</option>
              <option value="1440p">1440p (QHD)</option>
              <option value="2160p">4K (Ultra HD)</option>
            </select>
          </div>
        </div>

        <div className="grid grid-cols-1 sm:grid-cols-3 gap-4">
          <div className="space-y-1.5">
            <label
              htmlFor="stream-fps"
              className="block text-[10px] font-semibold uppercase tracking-[0.05em] text-gray-500"
            >
              Frame Rate
            </label>
            <select
              id="stream-fps"
              value={streamFps}
              onChange={(e) => setStreamFps(Number(e.target.value))}
              className="w-full rounded-lg bg-background/90 border border-gray-800 text-sm text-gray-200 py-2 px-3 focus:outline-none focus-visible:ring-2 focus-visible:ring-safelight/70 focus-visible:ring-offset-2 focus-visible:ring-offset-background cursor-pointer"
            >
              <option value={15}>15 fps</option>
              <option value={24}>24 fps</option>
              <option value={30}>30 fps</option>
              <option value={60}>60 fps</option>
            </select>
          </div>

          <div className="space-y-1.5">
            <label
              htmlFor="stream-bitrate"
              className="block text-[10px] font-semibold uppercase tracking-[0.05em] text-gray-500"
            >
              Bitrate Limit
            </label>
            <select
              id="stream-bitrate"
              value={bitrateLimit}
              onChange={(e) => setBitrateLimit(Number(e.target.value))}
              className="w-full rounded-lg bg-background/90 border border-gray-800 text-sm text-gray-200 py-2 px-3 focus:outline-none focus-visible:ring-2 focus-visible:ring-safelight/70 focus-visible:ring-offset-2 focus-visible:ring-offset-background cursor-pointer"
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

        <div className="space-y-1.5">
          <label
            htmlFor="api-endpoint"
            className="block text-[10px] font-semibold uppercase tracking-[0.05em] text-gray-500"
          >
            API Endpoint
          </label>
          <input
            id="api-endpoint"
            type="text"
            value={apiEndpoint}
            onChange={(e) => setApiEndpoint(e.target.value)}
            placeholder="http://localhost:3001"
            className="w-full rounded-lg bg-background/90 border border-gray-800 text-sm text-gray-200 py-2 px-3 focus:outline-none focus-visible:ring-2 focus-visible:ring-safelight/70 focus-visible:ring-offset-2 focus-visible:ring-offset-background font-mono"
          />
        </div>
      </div>
    </div>
  </div>
);
