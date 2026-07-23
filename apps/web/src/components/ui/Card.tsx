import React from 'react';
import { clsx } from 'clsx';
import { twMerge } from 'tailwind-merge';

export const Card: React.FC<{ children: React.ReactNode; className?: string }> = ({
  children,
  className,
}) => {
  return (
    <div
      className={twMerge(
        clsx(
          'bg-card/80 border border-border/80 rounded-xl p-6',
          className
        )
      )}
    >
      {children}
    </div>
  );
};
