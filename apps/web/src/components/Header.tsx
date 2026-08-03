import { ArrowLeft, ScreenShare } from 'lucide-react';
import type React from 'react';
import { Link } from 'react-router-dom';

export const Header: React.FC = () => {
  return (
    <header className="bg-background/80 border-b border-border/80 px-6 py-3 backdrop-blur-md sticky top-0 z-30">
      <div className="max-w-7xl mx-auto flex items-center justify-between gap-4">
        <div className="flex items-center gap-3">
          <Link
            to="/"
            className="p-1.5 text-muted-foreground hover:text-foreground hover:bg-secondary rounded-lg transition-colors mr-1 focus:outline-none focus-visible:ring-2 focus-visible:ring-safelight/70 focus-visible:ring-offset-2 focus-visible:ring-offset-background"
            title="Return to Home"
            aria-label="Return to Home"
          >
            <ArrowLeft className="w-5 h-5" aria-hidden="true" />
          </Link>
          <Link to="/" className="flex items-center gap-2.5 group">
            <div className="p-2 bg-secondary rounded-lg text-body-text group-hover:text-foreground transition-colors">
              <ScreenShare className="w-5 h-5" />
            </div>
            <span className="font-bold text-base text-foreground tracking-tight">Slopcast</span>
          </Link>
        </div>
      </div>
    </header>
  );
};
