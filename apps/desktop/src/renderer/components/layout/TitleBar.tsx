import { Copy, Minus, Settings, Square, X } from 'lucide-react';
import { AnimatePresence, motion } from 'motion/react';
import React, { useEffect, useState } from 'react';
import { windowControls } from '@/api/windowControls';
import appIcon from '../../../../src-tauri/icons/128x128.png';

export interface TitleBarProps {
  isLive?: boolean;
  isPreviewing?: boolean;
  title?: string;
  onSettings?: () => void;
}

function getCenterSignal(isPreviewing: boolean, title?: string): React.ReactNode {
  if (title) return <h1 className="text-sm font-semibold tracking-tight text-foreground/90">{title}</h1>;
  if (isPreviewing) {
    return (
      <span className="inline-flex items-center gap-2">
        <span className="size-1.5 rounded-full bg-muted-foreground/60" aria-hidden="true" />
        <span className="text-xs font-medium uppercase tracking-widest text-muted-foreground leading-none">
          Preview
        </span>
      </span>
    );
  }
  return null;
}

export const TitleBar: React.FC<TitleBarProps> = React.memo(
  ({ isLive = false, isPreviewing = false, title, onSettings }) => {
    const [maximized, setMaximized] = useState(false);
    const centerLayout = title ? 'absolute inset-0' : 'flex-1';

    const handlePointerDown = (event: React.PointerEvent<HTMLElement>): void => {
      if (event.button !== 0) return;
      if (event.target instanceof Element && event.target.closest('button')) return;
      if (event.detail === 2) {
        void windowControls.toggleMaximize();
        return;
      }
      windowControls.dragFromPointer(event);
    };

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
        onPointerDown={handlePointerDown}
        className="relative isolate h-10 shrink-0 flex items-stretch border-b border-border bg-background select-none"
      >
        <AnimatePresence>
          {isLive && (
            <motion.div
              key="live-signal"
              aria-hidden="true"
              className="pointer-events-none absolute inset-0 -z-10"
              initial={{ opacity: 0 }}
              animate={{ opacity: 1 }}
              exit={{ opacity: 0 }}
              transition={{ duration: 0.6, ease: 'easeOut' }}
            >
              <div className="size-full bg-live-signal opacity-60 motion-safe:animate-live-breathe" />
            </motion.div>
          )}
        </AnimatePresence>
        <div className="flex items-center gap-2.5 pl-4 pr-3">
          <img src={appIcon} alt="" draggable={false} className="size-4" />
          <span className="text-sm font-semibold tracking-tight text-foreground/90">Slopcast</span>
        </div>

        <div className={`${centerLayout} flex items-center justify-center pointer-events-none select-none`}>
          {getCenterSignal(isPreviewing, title)}
        </div>

        <div className="ml-auto flex items-stretch">
          {onSettings && (
            <button
              type="button"
              aria-label="Settings"
              title="Settings (Ctrl/Cmd+,)"
              onClick={onSettings}
              className="w-11 flex items-center justify-center text-muted-foreground transition-colors hover:bg-accent/60 hover:text-foreground focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-ring"
            >
              <Settings className="w-4 h-4" aria-hidden="true" />
            </button>
          )}
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
  },
);

TitleBar.displayName = 'TitleBar';
