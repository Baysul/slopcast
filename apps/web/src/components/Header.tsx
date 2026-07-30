import { ArrowLeft, Check, Copy, ScreenShare } from 'lucide-react';
import type React from 'react';
import { useState } from 'react';
import { Link } from 'react-router-dom';
import { Badge } from './ui/badge';

interface HeaderProps {
  roomCode?: string;
  shareUrl?: string;
  status?: 'connecting' | 'live' | 'disconnected' | 'closed' | 'error';
  statusText?: string;
}

export const Header: React.FC<HeaderProps> = ({ roomCode, shareUrl, status = 'connecting', statusText }) => {
  const [copied, setCopied] = useState(false);

  const handleCopyLink = () => {
    const url = shareUrl || window.location.href;
    navigator.clipboard.writeText(url).catch((err) => {
      console.warn('[Header] copy link failed:', err);
    });
    setCopied(true);
    setTimeout(() => setCopied(false), 2000);
  };

  const statusBadge = () => {
    switch (status) {
      case 'live':
        return <Badge variant="live">{statusText || 'Live Stream'}</Badge>;
      case 'connecting':
        return <Badge variant="info">{statusText || 'Connecting...'}</Badge>;
      case 'disconnected':
      case 'closed':
      case 'error':
        return <Badge variant="disconnected">{statusText || 'Disconnected'}</Badge>;
      default:
        return null;
    }
  };

  return (
    <header className="bg-background/80 border-b border-border/80 px-6 py-3 backdrop-blur-md sticky top-0 z-30">
      <div className="max-w-7xl mx-auto flex items-center justify-between gap-4">
        <div className="flex items-center gap-3">
          {roomCode && (
            <Link
              to="/"
              className="p-1.5 text-muted-foreground hover:text-foreground hover:bg-secondary rounded-lg transition-colors mr-1 focus:outline-none focus-visible:ring-2 focus-visible:ring-safelight/70 focus-visible:ring-offset-2 focus-visible:ring-offset-background"
              title="Return to Home"
              aria-label="Return to Home"
            >
              <ArrowLeft className="w-5 h-5" aria-hidden="true" />
            </Link>
          )}
          <Link to="/" className="flex items-center gap-2.5 group">
            <div className="p-2 bg-secondary rounded-xl text-body-text group-hover:text-foreground transition-colors">
              <ScreenShare className="w-5 h-5" />
            </div>
            <span className="font-bold text-base text-foreground tracking-tight">Slopcast</span>
          </Link>
        </div>

        {roomCode && (
          <div className="flex items-center gap-3">
            {statusBadge()}

            <button
              type="button"
              onClick={handleCopyLink}
              className="flex items-center gap-1.5 px-2.5 py-1.5 text-xs text-muted-foreground hover:text-foreground hover:bg-secondary rounded-lg transition-colors focus:outline-none focus-visible:ring-2 focus-visible:ring-safelight/70 focus-visible:ring-offset-2 focus-visible:ring-offset-background"
              title="Copy room link"
              aria-label={copied ? 'Link copied' : 'Copy room link'}
            >
              {copied ? <Check className="w-3.5 h-3.5" /> : <Copy className="w-3.5 h-3.5" />}
            </button>
          </div>
        )}
      </div>
    </header>
  );
};
