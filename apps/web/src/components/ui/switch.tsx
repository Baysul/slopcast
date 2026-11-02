import { Root as SwitchPrimitivesRoot, Thumb as SwitchPrimitivesThumb } from '@radix-ui/react-switch';
import type { SwitchProps } from '@radix-ui/react-switch';
import { forwardRef } from 'react';
import type { ComponentRef } from 'react';

import { cn } from '@/lib/utils';

const Switch = forwardRef<ComponentRef<typeof SwitchPrimitivesRoot>, SwitchProps>(
  ({ className, ...props }: SwitchProps, ref) => (
    <SwitchPrimitivesRoot
      ref={ref}
      className={cn(
        'peer inline-flex h-6 w-11 shrink-0 cursor-pointer items-center rounded-full border-2 border-transparent transition-colors focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2 focus-visible:ring-offset-black disabled:cursor-not-allowed disabled:opacity-50 data-[state=checked]:bg-primary data-[state=unchecked]:bg-input',
        className,
      )}
      {...props}
    >
      <SwitchPrimitivesThumb className="pointer-events-none block size-5 rounded-full bg-background shadow-lg ring-0 transition-transform data-[state=checked]:translate-x-5 data-[state=unchecked]:translate-x-0" />
    </SwitchPrimitivesRoot>
  ),
);
Switch.displayName = SwitchPrimitivesRoot.displayName;

export { Switch };
