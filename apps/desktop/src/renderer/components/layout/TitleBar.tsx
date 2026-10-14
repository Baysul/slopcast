import { AnimatePresence, motion } from 'motion/react';
import React, { useEffect, useRef, useState } from 'react';
import { windowControls } from '@/api/windowControls';
import appIcon from '../../../../src-tauri/icons/128x128.png';
import { WindowButtons } from './WindowButtons';

const DOUBLE_CLICK_MS = 400;
const DRAG_THRESHOLD_PX = 4;

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

    const pressRef = useRef<{ x: number; y: number; time: number; isArmed: boolean } | null>(null);

    // Starting the move on press would hand the release to the window manager, so a double-click
    // would never arrive. The move starts once the pointer actually travels.
    const handlePointerDown = (event: React.PointerEvent<HTMLElement>): void => {
      if (event.button !== 0) return;
      if (event.target instanceof Element && event.target.closest('button')) return;

      const previous = pressRef.current;
      const isDoubleClick =
        previous !== null &&
        event.timeStamp - previous.time < DOUBLE_CLICK_MS &&
        Math.hypot(event.clientX - previous.x, event.clientY - previous.y) < DRAG_THRESHOLD_PX;
      if (isDoubleClick) {
        pressRef.current = null;
        void windowControls.toggleMaximize();
        return;
      }

      pressRef.current = { x: event.clientX, y: event.clientY, time: event.timeStamp, isArmed: true };
    };

    const handlePointerMove = (event: React.PointerEvent<HTMLElement>): void => {
      const press = pressRef.current;
      if (press === null || !press.isArmed) return;
      if (Math.hypot(event.clientX - press.x, event.clientY - press.y) < DRAG_THRESHOLD_PX) return;

      press.isArmed = false;
      windowControls.startMove();
    };

    const handlePointerEnd = (): void => {
      if (pressRef.current) pressRef.current.isArmed = false;
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
        onPointerMove={handlePointerMove}
        onPointerUp={handlePointerEnd}
        onPointerCancel={handlePointerEnd}
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

        <WindowButtons maximized={maximized} onSettings={onSettings} />
      </header>
    );
  },
);

TitleBar.displayName = 'TitleBar';
