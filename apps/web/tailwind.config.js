/** @type {import('tailwindcss').Config} */
export default {
  content: ['./index.html', './src/**/*.{js,ts,jsx,tsx}'],
  theme: {
    extend: {
      colors: {
        background: '#090d16',
        foreground: '#f3f4f6',
        card: '#111827',
        'card-foreground': '#f9fafb',
        popover: '#111827',
        'popover-foreground': '#f9fafb',
        primary: {
          DEFAULT: '#6366f1',
          foreground: '#ffffff',
          hover: '#4f46e5',
        },
        secondary: {
          DEFAULT: '#1f2937',
          foreground: '#f3f4f6',
        },
        muted: {
          DEFAULT: '#1f2937',
          foreground: '#9ca3af',
        },
        accent: {
          DEFAULT: '#374151',
          foreground: '#f3f4f6',
        },
        destructive: {
          DEFAULT: '#ef4444',
          foreground: '#ffffff',
        },
        border: '#1f2937',
        input: '#374151',
        ring: '#6366f1',
      },
    },
  },
  plugins: [],
};
