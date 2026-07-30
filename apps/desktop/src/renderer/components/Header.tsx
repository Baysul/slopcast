import { ScreenShare } from 'lucide-react';
import type React from 'react';
import { Badge } from './ui/badge';

export interface HeaderProps {
  roomCode: string;
  isSharing: boolean;
  spectatorCount: number;
  copied: 'link' | 'code' | null;
  handleCreateRoom: () => void;
  handleCopyLink: () => void;
  handleCopyCode: () => void;
}

export const Header: React.FC<HeaderProps> = ({
  roomCode,
  isSharing,
  spectatorCount,
  copied,
  handleCreateRoom,
  handleCopyLink,
  handleCopyCode,
}) => (
  <header className="sticky top-0 z-10 border-b border-gray-800 bg-background/80 backdrop-blur-md">
    <div className="max-w-5xl mx-auto px-6 h-14 flex items-center justify-between gap-4">
      <div className="flex items-center gap-3 min-w-0">
        <span className="p-2 bg-secondary rounded-xl text-body-text shrink-0">
          <ScreenShare className="w-5 h-5" aria-hidden="true" />
        </span>
        <h1 className="text-lg font-bold text-gray-100 shrink-0 tracking-tight">Slopcast</h1>
        {isSharing && (
          <span role="status" aria-live="polite">
            <Badge variant="live">
              <span className="relative w-1.5 h-1.5 shrink-0" aria-hidden="true">
                <span className="absolute inset-0 rounded-full bg-safelight animate-ping opacity-75" />
                <span className="absolute inset-0 rounded-full bg-safelight" />
              </span>
              LIVE
            </Badge>
          </span>
        )}
      </div>

      <div className="shrink-0">
        {!roomCode ? (
          <button
            type="button"
            onClick={handleCreateRoom}
            className="px-5 py-2 bg-safelight text-safelight-foreground rounded-lg font-semibold text-sm hover:bg-safelight-hover transition-colors focus:outline-none focus-visible:ring-2 focus-visible:ring-safelight focus-visible:ring-offset-2 focus-visible:ring-offset-background"
          >
            Create Live Room
          </button>
        ) : (
          <div className="flex items-center gap-2">
            {spectatorCount > 0 && (
              <span className="hidden sm:inline-flex items-center rounded-full px-2.5 py-1 text-xs font-medium text-muted-foreground bg-gray-900/80 border border-accent shrink-0 tabular-nums">
                {spectatorCount} spectator{spectatorCount === 1 ? '' : 's'}
              </span>
            )}
            <button
              type="button"
              onClick={handleCopyCode}
              className="flex items-center gap-2 bg-gray-900/80 border border-gray-800 px-3 py-1.5 rounded-lg text-xs focus:outline-none focus-visible:ring-2 focus-visible:ring-safelight/70 focus-visible:ring-offset-2 focus-visible:ring-offset-background transition-colors"
            >
              <span className="text-gray-400 font-mono">{roomCode}</span>
              <span className="text-gray-200 bg-accent/50 px-2 py-0.5 rounded">
                {copied === 'code' ? 'Copied' : 'Copy'}
              </span>
            </button>
            <button
              type="button"
              onClick={handleCopyLink}
              className="bg-safelight text-safelight-foreground px-3 py-1.5 rounded-lg text-xs font-semibold hover:bg-safelight-hover transition-colors focus:outline-none focus-visible:ring-2 focus-visible:ring-safelight focus-visible:ring-offset-2 focus-visible:ring-offset-background"
            >
              {copied === 'link' ? 'Link Copied!' : 'Copy Link'}
            </button>
          </div>
        )}
      </div>
    </div>
  </header>
);
