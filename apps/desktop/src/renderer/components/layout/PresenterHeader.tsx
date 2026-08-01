import { Check, Copy, ScreenShare } from 'lucide-react';
import type React from 'react';
import { Badge } from '@/components/ui/badge';
import { Button } from '@/components/ui/button';

export interface PresenterHeaderProps {
  roomCode: string;
  shareUrl: string;
  spectatorCount: number;
  isCreatingRoom: boolean;
  copied: 'link' | 'code' | null;
  onCreateRoom: () => void;
  onCopyCode: () => void;
  onCopyLink: () => void;
}

export const PresenterHeader: React.FC<PresenterHeaderProps> = ({
  roomCode,
  spectatorCount,
  isCreatingRoom,
  copied,
  onCreateRoom,
  onCopyCode,
  onCopyLink,
}) => {
  return (
    <header className="sticky top-0 z-10 border-b border-border bg-background/80 backdrop-blur-md">
      <div className="max-w-5xl mx-auto px-6 h-14 flex items-center justify-between gap-4">
        <div className="flex items-center gap-3 min-w-0">
          <span className="p-2 bg-safelight/10 rounded-xl text-safelight shrink-0">
            <ScreenShare className="w-5 h-5" aria-hidden="true" />
          </span>
          <h1 className="text-xl font-bold text-foreground shrink-0 leading-tight tracking-tight">Slopcast</h1>
        </div>

        <div className="shrink-0">
          {!roomCode ? (
            <Button onClick={onCreateRoom} disabled={isCreatingRoom}>
              {isCreatingRoom ? 'Creating Room...' : 'Create Live Room'}
            </Button>
          ) : (
            <div className="flex items-center gap-2">
              {spectatorCount > 0 && (
                <Badge variant="info" className="hidden sm:inline-flex tabular-nums">
                  {spectatorCount} spectator{spectatorCount === 1 ? '' : 's'}
                </Badge>
              )}
              <Button variant="secondary" size="sm" onClick={onCopyCode} className="gap-2">
                <span className="font-mono text-sm font-semibold tabular-nums tracking-wide text-foreground/90">
                  {roomCode}
                </span>
                <span className="text-foreground bg-accent/50 px-2 py-0.5 rounded-md text-xs flex items-center gap-1">
                  {copied === 'code' ? (
                    <>
                      <Check className="w-3 h-3 text-safelight" aria-hidden="true" />
                      Copied
                    </>
                  ) : (
                    'Copy'
                  )}
                </span>
              </Button>
              <Button size="sm" onClick={onCopyLink} className="gap-1.5">
                {copied === 'link' ? (
                  <>
                    <Check className="w-4 h-4" aria-hidden="true" />
                    Link Copied!
                  </>
                ) : (
                  <>
                    <Copy className="w-4 h-4" aria-hidden="true" />
                    Copy Link
                  </>
                )}
              </Button>
            </div>
          )}
        </div>
      </div>
    </header>
  );
};
