import { defineConfig } from 'vite'
import react from '@vitejs/plugin-react-swc'

export default defineConfig({
  plugins: [react()],
  resolve: {
    alias: {
      '@': '/src',
    },
  },
  server: {
    host: '127.0.0.1',
    port: 9026,
    strictPort: true,
    proxy: {
      '/api': {
        target: 'http://127.0.0.1:9022',
        changeOrigin: true,
      },
    },
  },
})
