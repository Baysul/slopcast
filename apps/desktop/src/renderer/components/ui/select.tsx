import {
  Content as SelectPrimitiveContent,
  Group as SelectPrimitiveGroup,
  Icon as SelectPrimitiveIcon,
  Item as SelectPrimitiveItem,
  ItemIndicator as SelectPrimitiveItemIndicator,
  ItemText as SelectPrimitiveItemText,
  Label as SelectPrimitiveLabel,
  Portal as SelectPrimitivePortal,
  Root as SelectPrimitiveRoot,
  ScrollDownButton as SelectPrimitiveScrollDownButton,
  ScrollUpButton as SelectPrimitiveScrollUpButton,
  Separator as SelectPrimitiveSeparator,
  Trigger as SelectPrimitiveTrigger,
  Value as SelectPrimitiveValue,
  Viewport as SelectPrimitiveViewport,
} from '@radix-ui/react-select';
import type {
  SelectContentProps,
  SelectItemProps,
  SelectLabelProps,
  SelectScrollDownButtonProps,
  SelectScrollUpButtonProps,
  SelectSeparatorProps,
  SelectTriggerProps,
} from '@radix-ui/react-select';
import { Check, ChevronDown, ChevronUp } from 'lucide-react';
import { forwardRef } from 'react';
import type { ComponentRef } from 'react';

import { cn } from '@/lib/utils';

const Select = SelectPrimitiveRoot;

const SelectGroup = SelectPrimitiveGroup;

const SelectValue = SelectPrimitiveValue;

const SelectTrigger = forwardRef<ComponentRef<typeof SelectPrimitiveTrigger>, SelectTriggerProps>(
  ({ className, children, ...props }: SelectTriggerProps, ref) => (
    <SelectPrimitiveTrigger
      ref={ref}
      className={cn(
        'flex h-10 w-full items-center justify-between rounded-md border border-input bg-background px-3 py-2 text-sm ring-offset-background data-[placeholder]:text-muted-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2 disabled:cursor-not-allowed disabled:opacity-50 [&>span]:line-clamp-1',
        className,
      )}
      {...props}
    >
      {children}
      <SelectPrimitiveIcon asChild>
        <ChevronDown className="h-4 w-4 opacity-50" />
      </SelectPrimitiveIcon>
    </SelectPrimitiveTrigger>
  ),
);
SelectTrigger.displayName = SelectPrimitiveTrigger.displayName;

const SelectScrollUpButton = forwardRef<ComponentRef<typeof SelectPrimitiveScrollUpButton>, SelectScrollUpButtonProps>(
  ({ className, ...props }: SelectScrollUpButtonProps, ref) => (
    <SelectPrimitiveScrollUpButton
      ref={ref}
      className={cn('flex cursor-default items-center justify-center py-1', className)}
      {...props}
    >
      <ChevronUp className="h-4 w-4" />
    </SelectPrimitiveScrollUpButton>
  ),
);
SelectScrollUpButton.displayName = SelectPrimitiveScrollUpButton.displayName;

const SelectScrollDownButton = forwardRef<
  ComponentRef<typeof SelectPrimitiveScrollDownButton>,
  SelectScrollDownButtonProps
>(({ className, ...props }: SelectScrollDownButtonProps, ref) => (
  <SelectPrimitiveScrollDownButton
    ref={ref}
    className={cn('flex cursor-default items-center justify-center py-1', className)}
    {...props}
  >
    <ChevronDown className="h-4 w-4" />
  </SelectPrimitiveScrollDownButton>
));
SelectScrollDownButton.displayName = SelectPrimitiveScrollDownButton.displayName;

const SelectContent = forwardRef<ComponentRef<typeof SelectPrimitiveContent>, SelectContentProps>(
  ({ className, children, position = 'popper', ...props }: SelectContentProps, ref) => (
    <SelectPrimitivePortal>
      <SelectPrimitiveContent
        ref={ref}
        className={cn(
          'relative z-50 max-h-[--radix-select-content-available-height] min-w-[8rem] overflow-y-auto overflow-x-hidden rounded-md border border-border/60 bg-popover text-popover-foreground shadow-lg data-[state=open]:animate-in data-[state=closed]:animate-out data-[state=closed]:fade-out-0 data-[state=open]:fade-in-0 data-[state=closed]:zoom-out-95 data-[state=open]:zoom-in-95 data-[side=bottom]:slide-in-from-top-2 data-[side=left]:slide-in-from-right-2 data-[side=right]:slide-in-from-left-2 data-[side=top]:slide-in-from-bottom-2 origin-[--radix-select-content-transform-origin]',
          position === 'popper' &&
            'data-[side=bottom]:translate-y-1 data-[side=left]:-translate-x-1 data-[side=right]:translate-x-1 data-[side=top]:-translate-y-1',
          className,
        )}
        position={position}
        {...props}
      >
        <SelectScrollUpButton />
        <SelectPrimitiveViewport
          className={cn(
            'p-1',
            position === 'popper' &&
              'h-[var(--radix-select-trigger-height)] w-full min-w-[var(--radix-select-trigger-width)]',
          )}
        >
          {children}
        </SelectPrimitiveViewport>
        <SelectScrollDownButton />
      </SelectPrimitiveContent>
    </SelectPrimitivePortal>
  ),
);
SelectContent.displayName = SelectPrimitiveContent.displayName;

const SelectLabel = forwardRef<ComponentRef<typeof SelectPrimitiveLabel>, SelectLabelProps>(
  ({ className, ...props }: SelectLabelProps, ref) => (
    <SelectPrimitiveLabel ref={ref} className={cn('py-1.5 pl-8 pr-2 text-sm font-semibold', className)} {...props} />
  ),
);
SelectLabel.displayName = SelectPrimitiveLabel.displayName;

const SelectItem = forwardRef<ComponentRef<typeof SelectPrimitiveItem>, SelectItemProps>(
  ({ className, children, ...props }: SelectItemProps, ref) => (
    <SelectPrimitiveItem
      ref={ref}
      className={cn(
        'relative flex w-full cursor-pointer select-none items-center rounded-sm py-1.5 pl-8 pr-2 text-sm outline-none focus:bg-accent focus:text-accent-foreground data-[disabled]:pointer-events-none data-[disabled]:opacity-50',
        className,
      )}
      {...props}
    >
      <span className="absolute left-2 flex h-3.5 w-3.5 items-center justify-center">
        <SelectPrimitiveItemIndicator>
          <Check className="h-4 w-4" />
        </SelectPrimitiveItemIndicator>
      </span>

      <SelectPrimitiveItemText>{children}</SelectPrimitiveItemText>
    </SelectPrimitiveItem>
  ),
);
SelectItem.displayName = SelectPrimitiveItem.displayName;

const SelectSeparator = forwardRef<ComponentRef<typeof SelectPrimitiveSeparator>, SelectSeparatorProps>(
  ({ className, ...props }: SelectSeparatorProps, ref) => (
    <SelectPrimitiveSeparator ref={ref} className={cn('-mx-1 my-1 h-px bg-muted', className)} {...props} />
  ),
);
SelectSeparator.displayName = SelectPrimitiveSeparator.displayName;

export {
  Select,
  SelectContent,
  SelectGroup,
  SelectItem,
  SelectLabel,
  SelectScrollDownButton,
  SelectScrollUpButton,
  SelectSeparator,
  SelectTrigger,
  SelectValue,
};
