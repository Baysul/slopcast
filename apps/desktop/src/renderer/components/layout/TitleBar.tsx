import { Copy, Minus, ScreenShare, Square, X } from 'lucide-react';
import React, { useEffect, useState } from 'react';
import { windowControls } from '@/api/windowControls';

// Custom window chrome (Tauri window-customization guide): the native titlebar
// is disabled (`decorations: false`), so this bar owns the drag region and the
// minimize/maximize/close controls. `data-tauri-drag-region` is applied only
// to the bar and the branding block — the control buttons deliberately omit it
// so they receive clicks, and the OS native drag path gives double-click
// maximize for free. The branding block uses `deep` so its icon and label
// children (which would otherwise swallow the mousedown) stay draggable.
export const TitleBar: React.FC = React.memo(() => {
  const [maximized, setMaximized] = useState(false);

  useEffect(() => {
    let disposed = false;
    let unlisten: (() => void) | null = null;
    const refresh = async (): Promise<void> => {
      const value = await windowControls.isMaximized();
      if (!disposed) setMaximized(value);
    };
    void refresh();
    void windowControls
      .onResized(() => void refresh())
      .then((fn) => {
        unlisten = fn;
        if (disposed) void fn();
      });
    return () => {
      disposed = true;
      unlisten?.();
    };
  }, []);

  return (
    <header
      data-tauri-drag-region
      className="h-10 shrink-0 flex items-stretch border-b border-border bg-background select-none"
    >
      <div data-tauri-drag-region="deep" className="flex items-center gap-2.5 pl-4 pr-3">
        <span className="p-1.5 bg-safelight/10 rounded-lg text-safelight">
          <ScreenShare className="w-4 h-4" aria-hidden="true" />
        </span>
        <span className="text-sm font-semibold tracking-tight text-foreground/90">Slopcast</span>
      </div>

      <div className="ml-auto flex items-stretch">
        <button
          type="button"
          aria-label="Minimize"
          onClick={() => void windowControls.minimize()}
          className="w-11 flex items-center justify-center text-muted-foreground transition-colors hover:bg-accent/60 hover:text-foreground"
        >
          <Minus className="w-4 h-4" aria-hidden="true" />
        </button>
        <button
          type="button"
          aria-label={maximized ? 'Restore' : 'Maximize'}
          onClick={() => void windowControls.toggleMaximize()}
          className="w-11 flex items-center justify-center text-muted-foreground transition-colors hover:bg-accent/60 hover:text-foreground"
        >
          {maximized ? (
            <Copy className="w-3.5 h-3.5" aria-hidden="true" />
          ) : (
            <Square className="w-3.5 h-3.5" aria-hidden="true" />
          )}
        </button>
        <button
          type="button"
          aria-label="Close"
          onClick={() => void windowControls.close()}
          className="w-11 flex items-center justify-center text-muted-foreground transition-colors hover:bg-destructive hover:text-destructive-foreground"
        >
          <X className="w-4 h-4" aria-hidden="true" />
        </button>
      </div>
    </header>
  );
});

TitleBar.displayName = 'TitleBar';
