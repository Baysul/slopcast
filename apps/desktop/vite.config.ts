import path from 'node:path';

import tailwindcss from '@tailwindcss/vite';
import react from '@vitejs/plugin-react';
import { defineConfig, lazyPlugins } from 'vite-plus';

export default defineConfig({
  base: './',
  build: {
    cssMinify: true,
    emptyOutDir: true,
    minify: 'oxc',
    outDir: path.resolve(import.meta.dirname, 'dist/renderer'),
    reportCompressedSize: false,
    rolldownOptions: {
      output: {
        codeSplitting: {
          groups: [
            { name: 'vendor-react', test: /node_modules\/(?:react|react-dom|scheduler)\//u },
            { name: 'vendor-tauri', test: /node_modules\/@tauri-apps\//u },
            {
              name: 'vendor-ui',
              test: /node_modules\/(?:@radix-ui|motion|motion-dom|motion-utils|framer-motion|lucide-react|sonner|class-variance-authority|clsx|tailwind-merge)\//u,
            },
          ],
        },
        minify: {
          codegen: { legalComments: 'none' },
          compress: { dropDebugger: true },
        },
      },
    },
    sourcemap: false,
    target: 'esnext',
  },
  plugins: lazyPlugins(() => [tailwindcss(), react()]),
  resolve: {
    alias: {
      '@': path.resolve(import.meta.dirname, './src/renderer'),
    },
  },
  root: path.resolve(import.meta.dirname, 'src/renderer'),
  server: {
    port: 5173,
    strictPort: true,
  },
});
