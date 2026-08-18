import path from 'node:path';
import tailwindcss from '@tailwindcss/vite';
import react from '@vitejs/plugin-react';
import { defineConfig } from 'vite';

export default defineConfig({
  plugins: [tailwindcss(), react()],
  root: path.resolve(__dirname, 'src/renderer'),
  base: './',
  resolve: {
    alias: {
      '@': path.resolve(__dirname, './src/renderer'),
    },
  },
  build: {
    outDir: path.resolve(__dirname, 'dist/renderer'),
    emptyOutDir: true,
    sourcemap: false,
    minify: 'esbuild',
    cssMinify: true,
    reportCompressedSize: false,
    target: 'esnext',
    rollupOptions: {
      treeshake: {
        preset: 'recommended',
      },
      output: {
        manualChunks: {
          'vendor-react': ['react', 'react-dom', 'react-dom/client'],
          'vendor-tauri': ['@tauri-apps/api', '@tauri-apps/plugin-clipboard-manager'],
          'vendor-motion': ['motion/react'],
          'vendor-radix': ['@radix-ui/react-select', '@radix-ui/react-slot', '@radix-ui/react-switch'],
          'vendor-ui': ['lucide-react', 'sonner', 'class-variance-authority', 'clsx', 'tailwind-merge'],
        },
      },
    },
  },
  esbuild: {
    legalComments: 'none',
    drop: ['debugger'],
  },
  server: {
    port: 5173,
    strictPort: true,
  },
});
