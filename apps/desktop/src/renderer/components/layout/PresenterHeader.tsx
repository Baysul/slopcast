import { Check, Copy } from 'lucide-react';
import React from 'react';
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

export const PresenterHeader: React.FC<PresenterHeaderProps> = React.memo(
  ({ roomCode, spectatorCount, isCreatingRoom, copied, onCreateRoom, onCopyCode, onCopyLink }) => {
    // Room-controls toolbar below the custom titlebar (which owns the
    // branding); a static row in the app shell, so no sticky/backdrop needed.
    return (
      <header className="shrink-0 border-b border-border bg-background">
        <div className="max-w-5xl mx-auto px-6 h-14 flex items-center justify-end gap-4">
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
  },
);

PresenterHeader.displayName = 'PresenterHeader';
