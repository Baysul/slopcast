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
          'bg-gray-900/80 border border-gray-800/80 rounded-xl p-6 backdrop-blur-md shadow-xl',
          className
        )
      )}
    >
      {children}
    </div>
  );
};
