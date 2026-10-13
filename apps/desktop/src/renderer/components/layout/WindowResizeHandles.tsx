import type React from 'react';
import { type ResizeDirection, windowControls } from '@/api/windowControls';

const HANDLES: readonly { direction: ResizeDirection; className: string }[] = [
  { direction: 'North', className: 'top-0 left-2.5 right-2.5 h-1.5 cursor-ns-resize' },
  { direction: 'South', className: 'bottom-0 left-2.5 right-2.5 h-1.5 cursor-ns-resize' },
  { direction: 'West', className: 'left-0 top-2.5 bottom-2.5 w-1.5 cursor-ew-resize' },
  { direction: 'East', className: 'right-0 top-2.5 bottom-2.5 w-1.5 cursor-ew-resize' },
  { direction: 'NorthWest', className: 'left-0 top-0 size-2.5 cursor-nwse-resize' },
  { direction: 'NorthEast', className: 'right-0 top-0 size-2.5 cursor-nesw-resize' },
  { direction: 'SouthWest', className: 'left-0 bottom-0 size-2.5 cursor-nesw-resize' },
  { direction: 'SouthEast', className: 'right-0 bottom-0 size-2.5 cursor-nwse-resize' },
];

export const WindowResizeHandles: React.FC = () => (
  <>
    {HANDLES.map(({ direction, className }) => (
      <div
        key={direction}
        aria-hidden="true"
        className={`absolute z-20 ${className}`}
        onPointerDown={(event) => {
          if (event.button !== 0) return;
          event.preventDefault();
          windowControls.resizeFromPointer(direction, event);
        }}
      />
    ))}
  </>
);
