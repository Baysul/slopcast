/** @type {import('tailwindcss').Config} */
export default {
  content: ['./src/renderer/**/*.{js,ts,jsx,tsx,html}'],
  theme: {
    extend: {
      colors: {
        background: '#090d16',
        foreground: '#f3f4f6',
        card: '#111827',
        'card-foreground': '#f9fafb',
        popover: '#111827',
        'popover-foreground': '#f9fafb',
        safelight: {
          DEFAULT: '#C4804A',
          hover: '#B0703E',
          foreground: '#090D16',
          glow: 'rgba(196, 128, 74, 0.15)',
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
          DEFAULT: '#e11d48',
          foreground: '#ffffff',
        },
        border: '#1f2937',
        input: '#374151',
        ring: '#C4804A',
        'body-text': '#d1d5db',
      },
      fontFamily: {
        display: ['system-ui', '-apple-system', 'BlinkMacSystemFont', '"Segoe UI"', 'Roboto', 'sans-serif'],
      },
      keyframes: {
        'toast-in': {
          '0%': { opacity: '0', transform: 'translateX(1rem) scale(0.96)' },
          '100%': { opacity: '1', transform: 'translateX(0) scale(1)' },
        },
      },
      animation: {
        'toast-in': 'toast-in 0.22s cubic-bezier(0.16, 1, 0.3, 1)',
      },
    },
  },
  plugins: [],
};
