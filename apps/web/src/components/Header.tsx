import React, { useState } from 'react';
import { ScreenShare, Copy, Check, ShieldAlert, ArrowLeft } from 'lucide-react';
import { Link } from 'react-router-dom';
import { Badge } from './ui/Badge';
import { Button } from './ui/Button';

interface HeaderProps {
  roomCode?: string;
  shareUrl?: string;
  status?: 'connecting' | 'live' | 'disconnected' | 'closed' | 'error';
  statusText?: string;
}

export const Header: React.FC<HeaderProps> = ({
  roomCode,
  shareUrl,
  status = 'connecting',
  statusText,
}) => {
  const [copied, setCopied] = useState(false);

  const handleCopyLink = () => {
    const url = shareUrl || window.location.href;
    navigator.clipboard.writeText(url).catch(() => {});
    setCopied(true);
    setTimeout(() => setCopied(false), 2000);
  };

  const statusBadge = () => {
    switch (status) {
      case 'live':
        return <Badge variant="live">{statusText || 'Live Stream'}</Badge>;
      case 'connecting':
        return <Badge variant="connecting">{statusText || 'Connecting...'}</Badge>;
      case 'disconnected':
      case 'closed':
      case 'error':
        return <Badge variant="disconnected">{statusText || 'Disconnected'}</Badge>;
      default:
        return null;
    }
  };

  return (
    <header className="bg-gray-950/80 border-b border-gray-800/80 px-6 py-3.5 backdrop-blur-md sticky top-0 z-30">
      <div className="max-w-7xl mx-auto flex items-center justify-between gap-4">
        {/* Logo and Brand */}
        <div className="flex items-center gap-3">
          {roomCode && (
            <Link
              to="/"
              className="p-1.5 text-gray-400 hover:text-gray-100 hover:bg-gray-800 rounded-lg transition-colors mr-1"
              title="Return to Home"
            >
              <ArrowLeft className="w-5 h-5" />
            </Link>
          )}
          <Link to="/" className="flex items-center gap-2.5 group">
            <div className="p-2 bg-gradient-to-tr from-indigo-600 to-indigo-500 rounded-xl shadow-lg text-white group-hover:scale-105 transition-transform">
              <ScreenShare className="w-5 h-5" />
            </div>
            <div>
              <span className="font-bold text-base text-gray-100 tracking-tight block">
                ScreenShare
              </span>
              <span className="text-[10px] uppercase font-semibold tracking-wider text-indigo-400 block -mt-1">
                Web Spectator
              </span>
            </div>
          </Link>
        </div>

        {/* Room Info and Actions */}
        {roomCode && (
          <div className="flex items-center gap-3">
            {statusBadge()}

            <div className="hidden sm:flex items-center gap-2 bg-gray-900 border border-gray-800 rounded-lg px-3 py-1.5">
              <span className="text-xs text-gray-400 font-mono">Code:</span>
              <span className="text-xs font-mono font-semibold text-gray-100">{roomCode}</span>
            </div>

            <Button
              size="sm"
              variant="outline"
              onClick={handleCopyLink}
              className="text-xs border-gray-800 bg-gray-900/90 hover:bg-gray-800"
            >
              {copied ? (
                <>
                  <Check className="w-3.5 h-3.5 text-emerald-400" />
                  <span className="text-emerald-400">Copied!</span>
                </>
              ) : (
                <>
                  <Copy className="w-3.5 h-3.5" />
                  <span>Copy Link</span>
                </>
              )}
            </Button>
          </div>
        )}
      </div>
    </header>
  );
};
