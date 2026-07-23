/** @type {import('tailwindcss').Config} */
export default {
  content: ['./src/renderer/**/*.{js,ts,jsx,tsx,html}'],
  theme: {
    extend: {
      colors: {
        background: '#090d16',
        foreground: '#f3f4f6',
      },
    },
  },
  plugins: [],
};
