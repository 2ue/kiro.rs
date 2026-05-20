import { defineConfig } from 'vite'
import react from '@vitejs/plugin-react-swc'
import path from 'node:path'

const apiProxyTarget = process.env.VITE_API_PROXY_TARGET ?? 'http://127.0.0.1:9022'

export default defineConfig({
  plugins: [react()],
  base: '/console/',
  resolve: {
    alias: {
      '@': path.resolve(__dirname, './src'),
    },
  },
  server: {
    port: 9023,
    strictPort: true,
    host: '127.0.0.1',
    proxy: {
      '/api': { target: apiProxyTarget, changeOrigin: true },
      '/v1': { target: apiProxyTarget, changeOrigin: true },
    },
  },
  build: {
    outDir: 'dist',
    emptyOutDir: true,
    sourcemap: true,
  },
})
