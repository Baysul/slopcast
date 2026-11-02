import {
  Range as SliderPrimitiveRange,
  Root as SliderPrimitiveRoot,
  Thumb as SliderPrimitiveThumb,
  Track as SliderPrimitiveTrack,
} from '@radix-ui/react-slider';
import type { SliderProps } from '@radix-ui/react-slider';
import { forwardRef } from 'react';
import type { ComponentRef } from 'react';

import { cn } from '@/lib/utils';

const Slider = forwardRef<ComponentRef<typeof SliderPrimitiveRoot>, SliderProps>(
  ({ className, ...props }: SliderProps, ref) => (
    <SliderPrimitiveRoot
      ref={ref}
      className={cn(
        'relative flex w-full touch-none select-none items-center data-[disabled]:cursor-not-allowed data-[disabled]:opacity-50',
        className,
      )}
      {...props}
    >
      <SliderPrimitiveTrack className="relative h-1.5 w-full grow overflow-hidden rounded-full bg-input">
        <SliderPrimitiveRange className="absolute h-full bg-safelight" />
      </SliderPrimitiveTrack>
      <SliderPrimitiveThumb className="block size-5 rounded-full border-2 border-safelight bg-background shadow-sm transition-colors focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2 focus-visible:ring-offset-background disabled:pointer-events-none" />
    </SliderPrimitiveRoot>
  ),
);
Slider.displayName = SliderPrimitiveRoot.displayName;

export { Slider };
