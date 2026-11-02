import type React from 'react';

import { windowControls } from '@/api/windowControls';
import type { ResizeDirection } from '@/api/windowControls';

const HANDLES: readonly { readonly direction: ResizeDirection; readonly className: string }[] = [
  { className: 'top-0 left-2.5 right-2.5 h-1.5 cursor-ns-resize', direction: 'North' },
  { className: 'bottom-0 left-2.5 right-2.5 h-1.5 cursor-ns-resize', direction: 'South' },
  { className: 'left-0 top-2.5 bottom-2.5 w-1.5 cursor-ew-resize', direction: 'West' },
  { className: 'right-0 top-2.5 bottom-2.5 w-1.5 cursor-ew-resize', direction: 'East' },
  { className: 'left-0 top-0 size-2.5 cursor-nwse-resize', direction: 'NorthWest' },
  { className: 'right-0 top-0 size-2.5 cursor-nesw-resize', direction: 'NorthEast' },
  { className: 'left-0 bottom-0 size-2.5 cursor-nesw-resize', direction: 'SouthWest' },
  { className: 'right-0 bottom-0 size-2.5 cursor-nwse-resize', direction: 'SouthEast' },
];

const PRIMARY_MOUSE_BUTTON = 0;

export const WindowResizeHandles: React.FC = () => (
  <>
    {HANDLES.map(({ direction, className }) => (
      <div
        key={direction}
        aria-hidden="true"
        className={`absolute z-20 ${className}`}
        onPointerDown={(event) => {
          if (event.button !== PRIMARY_MOUSE_BUTTON) {
            return;
          }
          event.preventDefault();
          windowControls.startResize(direction);
        }}
      />
    ))}
  </>
);
