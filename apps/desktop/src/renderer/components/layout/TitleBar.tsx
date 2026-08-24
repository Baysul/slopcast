import { Copy, Minus, ScreenShare, Square, X } from 'lucide-react';
import { AnimatePresence, motion } from 'motion/react';
import React, { useEffect, useState } from 'react';
import { windowControls } from '@/api/windowControls';

export interface TitleBarProps {
  isLive?: boolean;
  isPreviewing?: boolean;
  onClose?: () => void;
}

function getCenterSignal(isPreviewing: boolean): React.ReactNode {
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

export const TitleBar: React.FC<TitleBarProps> = React.memo(({ isLive = false, isPreviewing = false, onClose }) => {
  const [maximized, setMaximized] = useState(false);

  const handleMouseDown = (event: React.MouseEvent<HTMLElement>): void => {
    if (event.button !== 0) return;
    if (event.target instanceof Element && event.target.closest('button')) return;
    if (event.detail === 2) {
      void windowControls.toggleMaximize();
    } else {
      void windowControls.startDragging();
    }
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
    // biome-ignore lint/a11y/noStaticElementInteractions: window drag region is a pointer-only interaction; keyboard users use the window's native controls.
    <header
      onMouseDown={handleMouseDown}
      className="relative isolate h-10 shrink-0 flex items-stretch border-b border-border bg-background select-none"
    >
      <AnimatePresence>
        {isLive && (
          <motion.div
            key="live-glow"
            aria-hidden="true"
            className="pointer-events-none absolute inset-0 -z-10"
            initial={{ opacity: 0 }}
            animate={{ opacity: 1 }}
            exit={{ opacity: 0 }}
            transition={{ duration: 0.6, ease: 'easeOut' }}
          >
            <div className="size-full bg-live-glow motion-safe:animate-live-breathe" />
          </motion.div>
        )}
      </AnimatePresence>
      <div className="flex items-center gap-2.5 pl-4 pr-3">
        <ScreenShare className="w-4 h-4 text-muted-foreground" aria-hidden="true" />
        <span className="text-sm font-semibold tracking-tight text-foreground/90">Slopcast</span>
      </div>

      <div className="flex-1 flex items-center justify-center pointer-events-none select-none">
        {getCenterSignal(isPreviewing)}
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
          onClick={() => {
            onClose?.();
            void windowControls.close();
          }}
          className="w-11 flex items-center justify-center text-muted-foreground transition-colors hover:bg-destructive hover:text-destructive-foreground"
        >
          <X className="w-4 h-4" aria-hidden="true" />
        </button>
      </div>
    </header>
  );
});

TitleBar.displayName = 'TitleBar';
