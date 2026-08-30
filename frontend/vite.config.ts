import { defineConfig } from 'vite'
import react from '@vitejs/plugin-react'

// Standalone dev with a proxy to the registry binary; in production the binary serves
// `dist/` itself, which keeps the single-container promise of spec §10.2.
export default defineConfig({
  plugins: [react()],
  server: {
    port: 5173,
    proxy: {
      '/api': 'http://127.0.0.1:8080',
      '/.well-known': 'http://127.0.0.1:8080',
      '/sparql': 'http://127.0.0.1:8080',
    },
  },
  build: { outDir: 'dist', sourcemap: true },
  test: {
    environment: 'jsdom',
    globals: true,
    setupFiles: ['./src/test-setup.ts'],
  },
} as any)
