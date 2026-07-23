import React from 'react';
import { clsx } from 'clsx';
import { twMerge } from 'tailwind-merge';

export interface BadgeProps {
  children: React.ReactNode;
  variant?: 'live' | 'connecting' | 'disconnected' | 'info' | 'secondary';
  className?: string;
}

export const Badge: React.FC<BadgeProps> = ({ children, variant = 'info', className }) => {
  const baseStyles = 'inline-flex items-center gap-1.5 px-2.5 py-0.5 rounded-full text-xs font-semibold uppercase tracking-wider select-none';

  const variants = {
    live: 'bg-emerald-500/10 text-emerald-400 border border-emerald-500/20',
    connecting: 'bg-amber-500/10 text-amber-400 border border-amber-500/20',
    disconnected: 'bg-rose-500/10 text-rose-400 border border-rose-500/20',
    info: 'bg-indigo-500/10 text-indigo-400 border border-indigo-500/20',
    secondary: 'bg-gray-800 text-gray-400 border border-gray-700',
  };

  return (
    <span className={twMerge(clsx(baseStyles, variants[variant], className))}>
      {children}
    </span>
  );
};
