import type { ComponentProps } from 'react';
import { Toaster as SonnerToaster } from 'sonner';

type ToasterProps = ComponentProps<typeof SonnerToaster>;

const Toaster = ({ ...props }: ToasterProps) => (
  <SonnerToaster
    position="bottom-right"
    className="toaster group"
    toastOptions={{
      classNames: {
        actionButton: 'group-[.toast]:bg-primary group-[.toast]:text-primary-foreground',
        cancelButton: 'group-[.toast]:bg-secondary group-[.toast]:text-secondary-foreground',
        description: 'group-[.toast]:text-muted-foreground',
        toast:
          'group toast group-[.toaster]:bg-card group-[.toaster]:text-foreground group-[.toaster]:border-border/80 group-[.toaster]:shadow-2xl group-[.toaster]:rounded-xl font-sans',
      },
    }}
    {...props}
  />
);

export { Toaster };
