import React, { useState } from 'react';
import { Monitor, Info, ExternalLink, X } from 'lucide-react';
import { Button } from './ui/Button';

export const SpectatorBanner: React.FC = () => {
  const [dismissed, setDismissed] = useState(false);

  if (dismissed) {
    return null;
  }

  return (
    <div className="bg-gradient-to-r from-indigo-950/90 via-gray-900 to-indigo-950/90 border-b border-indigo-500/20 px-4 py-3 shadow-lg relative z-20 backdrop-blur-md">
      <div className="max-w-7xl mx-auto flex flex-col sm:flex-row items-center justify-between gap-3 text-sm">
        <div className="flex items-center gap-3">
          <div className="p-2 bg-indigo-500/20 rounded-lg text-indigo-400 shrink-0">
            <Monitor className="w-5 h-5" />
          </div>
          <div>
            <div className="flex items-center gap-2 font-medium text-gray-100">
              <span>Web Spectator Mode Active</span>
              <span className="px-2 py-0.5 text-[10px] font-semibold bg-indigo-500/20 text-indigo-300 rounded-full border border-indigo-500/30">
                View Only
              </span>
            </div>
            <p className="text-gray-300 text-xs mt-0.5">
              You are currently spectating in a web browser. To host or share your screen and application audio, please launch the Desktop App.
            </p>
          </div>
        </div>

        <div className="flex items-center gap-2 shrink-0 w-full sm:w-auto justify-end">
          <Button
            size="sm"
            variant="primary"
            className="text-xs bg-indigo-600 hover:bg-indigo-500 text-white gap-1.5"
            onClick={() => {
              alert('To present, download and launch the ScreenShare Desktop Client.');
            }}
          >
            <span>Get Desktop App</span>
            <ExternalLink className="w-3.5 h-3.5" />
          </Button>
          <button
            onClick={() => setDismissed(true)}
            className="p-1.5 text-gray-400 hover:text-gray-200 hover:bg-gray-800 rounded-lg transition-colors"
            title="Dismiss banner"
          >
            <X className="w-4 h-4" />
          </button>
        </div>
      </div>
    </div>
  );
};
