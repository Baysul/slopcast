'use client';

import {
  Content as PopoverPrimitiveContent,
  Portal as PopoverPrimitivePortal,
  Root as PopoverPrimitiveRoot,
  Trigger as PopoverPrimitiveTrigger,
} from '@radix-ui/react-popover';
import type { PopoverContentProps } from '@radix-ui/react-popover';
import { forwardRef } from 'react';
import type { ComponentRef } from 'react';

import { cn } from '@/lib/utils';

const Popover = PopoverPrimitiveRoot;

const PopoverTrigger = PopoverPrimitiveTrigger;

const DEFAULT_SIDE_OFFSET = 4;

const PopoverContent = forwardRef<ComponentRef<typeof PopoverPrimitiveContent>, PopoverContentProps>(
  ({ className, align = 'center', sideOffset = DEFAULT_SIDE_OFFSET, ...props }: PopoverContentProps, ref) => (
    <PopoverPrimitivePortal>
      <PopoverPrimitiveContent
        ref={ref}
        align={align}
        sideOffset={sideOffset}
        className={cn(
          'z-50 w-72 rounded-md border bg-popover p-4 text-popover-foreground shadow-md outline-none data-[state=open]:animate-in data-[state=closed]:animate-out data-[state=closed]:fade-out-0 data-[state=open]:fade-in-0 data-[state=closed]:zoom-out-95 data-[state=open]:zoom-in-95 data-[side=bottom]:slide-in-from-top-2 data-[side=left]:slide-in-from-right-2 data-[side=right]:slide-in-from-left-2 data-[side=top]:slide-in-from-bottom-2 origin-[--radix-popover-content-transform-origin]',
          className,
        )}
        {...props}
      />
    </PopoverPrimitivePortal>
  ),
);
PopoverContent.displayName = PopoverPrimitiveContent.displayName;

export { Popover, PopoverContent, PopoverTrigger };
