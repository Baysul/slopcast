import {
  Action as AlertDialogPrimitiveAction,
  Cancel as AlertDialogPrimitiveCancel,
  Content as AlertDialogPrimitiveContent,
  Description as AlertDialogPrimitiveDescription,
  Overlay as AlertDialogPrimitiveOverlay,
  Portal as AlertDialogPrimitivePortal,
  Root as AlertDialogPrimitiveRoot,
  Title as AlertDialogPrimitiveTitle,
  Trigger as AlertDialogPrimitiveTrigger,
} from '@radix-ui/react-alert-dialog';
import type {
  AlertDialogActionProps,
  AlertDialogCancelProps,
  AlertDialogContentProps,
  AlertDialogDescriptionProps,
  AlertDialogOverlayProps,
  AlertDialogTitleProps,
} from '@radix-ui/react-alert-dialog';
import { forwardRef } from 'react';
import type { ComponentRef, HTMLAttributes } from 'react';

import { buttonVariants } from '@/components/ui/button';
import { cn } from '@/lib/utils';

const AlertDialog = AlertDialogPrimitiveRoot;

const AlertDialogTrigger = AlertDialogPrimitiveTrigger;

const AlertDialogPortal = AlertDialogPrimitivePortal;

const AlertDialogOverlay = forwardRef<ComponentRef<typeof AlertDialogPrimitiveOverlay>, AlertDialogOverlayProps>(
  ({ className, ...props }, ref) => (
    <AlertDialogPrimitiveOverlay
      className={cn(
        'fixed inset-0 z-50 bg-black/80  data-[state=open]:animate-in data-[state=closed]:animate-out data-[state=closed]:fade-out-0 data-[state=open]:fade-in-0',
        className,
      )}
      {...props}
      ref={ref}
    />
  ),
);
AlertDialogOverlay.displayName = AlertDialogPrimitiveOverlay.displayName;

const AlertDialogContent = forwardRef<ComponentRef<typeof AlertDialogPrimitiveContent>, AlertDialogContentProps>(
  ({ className, ...props }, ref) => (
    <AlertDialogPortal>
      <AlertDialogOverlay />
      <AlertDialogPrimitiveContent
        ref={ref}
        className={cn(
          'fixed left-[50%] top-[50%] z-50 grid w-full max-w-lg translate-x-[-50%] translate-y-[-50%] gap-4 rounded-lg border border-border bg-card/95 p-6 shadow-lg backdrop-blur-md duration-200 data-[state=open]:animate-in data-[state=closed]:animate-out data-[state=closed]:fade-out-0 data-[state=open]:fade-in-0 data-[state=closed]:zoom-out-95 data-[state=open]:zoom-in-95 data-[state=closed]:slide-out-to-left-1/2 data-[state=closed]:slide-out-to-top-[48%] data-[state=open]:slide-in-from-left-1/2 data-[state=open]:slide-in-from-top-[48%] sm:rounded-lg',
          className,
        )}
        {...props}
      />
    </AlertDialogPortal>
  ),
);
AlertDialogContent.displayName = AlertDialogPrimitiveContent.displayName;

const AlertDialogHeader = ({ className, ...props }: HTMLAttributes<HTMLDivElement>) => (
  <div className={cn('flex flex-col space-y-2 text-center sm:text-left', className)} {...props} />
);
AlertDialogHeader.displayName = 'AlertDialogHeader';

const AlertDialogFooter = ({ className, ...props }: HTMLAttributes<HTMLDivElement>) => (
  <div className={cn('flex flex-col-reverse sm:flex-row sm:justify-end sm:space-x-2', className)} {...props} />
);
AlertDialogFooter.displayName = 'AlertDialogFooter';

const AlertDialogTitle = forwardRef<ComponentRef<typeof AlertDialogPrimitiveTitle>, AlertDialogTitleProps>(
  ({ className, ...props }, ref) => (
    <AlertDialogPrimitiveTitle ref={ref} className={cn('text-lg font-semibold', className)} {...props} />
  ),
);
AlertDialogTitle.displayName = AlertDialogPrimitiveTitle.displayName;

const AlertDialogDescription = forwardRef<
  ComponentRef<typeof AlertDialogPrimitiveDescription>,
  AlertDialogDescriptionProps
>(({ className, ...props }, ref) => (
  <AlertDialogPrimitiveDescription ref={ref} className={cn('text-sm text-muted-foreground', className)} {...props} />
));
AlertDialogDescription.displayName = AlertDialogPrimitiveDescription.displayName;

const AlertDialogAction = forwardRef<ComponentRef<typeof AlertDialogPrimitiveAction>, AlertDialogActionProps>(
  ({ className, ...props }, ref) => (
    <AlertDialogPrimitiveAction ref={ref} className={cn(buttonVariants(), className)} {...props} />
  ),
);
AlertDialogAction.displayName = AlertDialogPrimitiveAction.displayName;

const AlertDialogCancel = forwardRef<ComponentRef<typeof AlertDialogPrimitiveCancel>, AlertDialogCancelProps>(
  ({ className, ...props }, ref) => (
    <AlertDialogPrimitiveCancel
      ref={ref}
      className={cn(buttonVariants({ variant: 'outline' }), 'mt-2 sm:mt-0', className)}
      {...props}
    />
  ),
);
AlertDialogCancel.displayName = AlertDialogPrimitiveCancel.displayName;

export {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogOverlay,
  AlertDialogPortal,
  AlertDialogTitle,
  AlertDialogTrigger,
};
