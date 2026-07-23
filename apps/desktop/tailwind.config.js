/** @type {import('tailwindcss').Config} */
export default {
  content: ['./src/renderer/**/*.{js,ts,jsx,tsx,html}'],
  theme: {
    extend: {
      colors: {
        background: '#090d16',
        foreground: '#f3f4f6',
        safelight: {
          DEFAULT: '#e8883a',
          hover: '#d47a2e',
          glow: 'rgba(232, 136, 58, 0.15)',
        },
        destructive: '#e11d48',
      },
    },
  },
  plugins: [],
};
