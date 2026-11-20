import tailwindcss from '@tailwindcss/vite';
import { defineConfig } from 'vite';
import solid from 'vite-plugin-solid';
import { viteSingleFile } from 'vite-plugin-singlefile';
import viteCompression from 'vite-plugin-compression';
import { fileURLToPath, URL } from 'node:url';
import path from 'node:path';

const rootDir = import.meta.dirname;

export default defineConfig({
  plugins: [
    tailwindcss(),
    solid(),
    viteSingleFile(),
    viteCompression({
      algorithm: 'gzip',
      ext: '.gz',
      deleteOriginFile: true,
      filter: /index\.html$/i,
      threshold: 0,
    }),
  ],
  server: { proxy: { '/api': 'http://192.168.4.1' } },
  build: {
    target: 'esnext',
    outDir: path.resolve(rootDir, '../dist'),
    emptyOutDir: true,
    cssCodeSplit: false,
    rollupOptions: {
      output: {
        inlineDynamicImports: true,
        entryFileNames: 'assets/app.js',
        assetFileNames: (asset) =>
          asset.names.some((name) => name.endsWith('.css'))
            ? 'assets/app.css'
            : 'assets/[name][extname]',
      },
    },
  },
  resolve: { alias: { '~': fileURLToPath(new URL('./src', import.meta.url)) } },
});
