import React from 'react';
import { clsx } from 'clsx';
import { twMerge } from 'tailwind-merge';

export interface BadgeProps {
  children: React.ReactNode;
  variant?: 'live' | 'disconnected' | 'info';
  className?: string;
}

export const Badge: React.FC<BadgeProps> = ({ children, variant = 'info', className }) => {
  const baseStyles = 'inline-flex items-center gap-1.5 px-2.5 py-0.5 rounded-full text-xs font-semibold uppercase tracking-wider select-none';

  const variants = {
    live: 'bg-safelight-glow text-safelight border border-safelight/20',
    disconnected: 'bg-destructive/10 text-destructive border border-destructive/20',
    info: 'bg-secondary text-muted-foreground border border-accent',
  };

  return (
    <span className={twMerge(clsx(baseStyles, variants[variant], className))}>
      {children}
    </span>
  );
};
