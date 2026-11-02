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
  ({ className, 'aria-label': ariaLabel, 'aria-valuetext': ariaValueText, ...props }: SliderProps, ref) => (
    <SliderPrimitiveRoot
      ref={ref}
      data-slot="slider"
      className={cn(
        'relative flex w-full touch-none select-none items-center data-[disabled]:cursor-not-allowed data-[disabled]:opacity-50',
        className,
      )}
      {...props}
    >
      <SliderPrimitiveTrack
        data-slot="slider-track"
        className="relative h-1.5 w-full grow overflow-hidden rounded-full bg-input"
      >
        <SliderPrimitiveRange data-slot="slider-range" className="absolute h-full bg-primary" />
      </SliderPrimitiveTrack>
      <SliderPrimitiveThumb
        data-slot="slider-thumb"
        aria-label={ariaLabel}
        aria-valuetext={ariaValueText}
        className="block size-5 rounded-full border-2 border-primary bg-background transition-colors focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2 focus-visible:ring-offset-background disabled:pointer-events-none"
      />
    </SliderPrimitiveRoot>
  ),
);
Slider.displayName = SliderPrimitiveRoot.displayName;

export { Slider };
