import path from 'node:path';
import tailwindcss from '@tailwindcss/vite';
import react from '@vitejs/plugin-react';
import { defineConfig } from 'vite';

export default defineConfig({
  plugins: [tailwindcss(), react()],
  root: path.resolve(import.meta.dirname, 'src/renderer'),
  base: './',
  resolve: {
    alias: {
      '@': path.resolve(import.meta.dirname, './src/renderer'),
    },
  },
  build: {
    outDir: path.resolve(import.meta.dirname, 'dist/renderer'),
    emptyOutDir: true,
    sourcemap: false,
    minify: 'oxc',
    cssMinify: true,
    reportCompressedSize: false,
    target: 'esnext',
    rolldownOptions: {
      output: {
        minify: {
          compress: { dropDebugger: true },
          codegen: { legalComments: 'none' },
        },
        codeSplitting: {
          groups: [
            { name: 'vendor-react', test: /node_modules\/(react|react-dom|scheduler)\// },
            { name: 'vendor-tauri', test: /node_modules\/@tauri-apps\// },
            {
              name: 'vendor-ui',
              test: /node_modules\/(@radix-ui|motion|motion-dom|motion-utils|framer-motion|lucide-react|sonner|class-variance-authority|clsx|tailwind-merge)\//,
            },
          ],
        },
      },
    },
  },
  server: {
    port: 5173,
    strictPort: true,
  },
});
