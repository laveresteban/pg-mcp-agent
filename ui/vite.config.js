import { defineConfig } from 'vite'
import react from '@vitejs/plugin-react'

// Dev server proxies /api to the Rust api_server on :7878 so the browser can
// hit a single origin. `vite build` emits to ui/dist, which api_server serves.
export default defineConfig({
  plugins: [react()],
  server: {
    port: 5173,
    proxy: {
      '/api': 'http://localhost:7878',
    },
  },
})
