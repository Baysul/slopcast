import { clsx } from 'clsx';
import type React from 'react';
import { twMerge } from 'tailwind-merge';

export interface ButtonProps extends React.ButtonHTMLAttributes<HTMLButtonElement> {
  variant?: 'primary' | 'secondary' | 'outline' | 'ghost' | 'destructive';
  size?: 'sm' | 'md' | 'lg';
}

export const Button: React.FC<ButtonProps> = ({
  children,
  className,
  variant = 'primary',
  size = 'md',
  disabled,
  ...props
}) => {
  const baseStyles =
    'inline-flex items-center justify-center font-medium rounded-lg transition-colors focus:outline-none focus:ring-2 focus:ring-safelight focus:ring-offset-2 disabled:opacity-50 disabled:pointer-events-none select-none';

  const variants = {
    primary: 'bg-safelight text-safelight-foreground hover:bg-safelight-hover active:bg-[#9C6234]',
    secondary: 'bg-secondary text-foreground hover:bg-accent active:bg-card border border-accent',
    outline: 'border border-accent bg-transparent text-gray-200 hover:bg-secondary hover:text-white',
    ghost: 'bg-transparent text-body-text hover:bg-secondary hover:text-white',
    destructive: 'bg-destructive text-destructive-foreground hover:bg-[#c41534] active:bg-[#b21a3e] shadow-sm',
  };

  const sizes = {
    sm: 'px-3 py-1.5 text-xs gap-1.5',
    md: 'px-4 py-2 text-sm gap-2',
    lg: 'px-6 py-3 text-base gap-2.5',
  };

  return (
    <button
      className={twMerge(clsx(baseStyles, variants[variant], sizes[size], className))}
      disabled={disabled}
      {...props}
    >
      {children}
    </button>
  );
};
