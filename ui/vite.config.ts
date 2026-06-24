import { defineConfig } from 'vite'
import react from '@vitejs/plugin-react-swc'
import tailwindcss from '@tailwindcss/vite'
import path from 'path'
import { fileURLToPath } from 'url'

const __dirname = path.dirname(fileURLToPath(import.meta.url))

const apiProxyTarget = process.env.VITE_API_PROXY_TARGET ?? 'http://127.0.0.1:8080'

function manualChunks(id: string) {
  if (!id.includes('node_modules')) return undefined
  if (id.includes('/react-router') || id.includes('/@remix-run/')) return 'vendor-router'
  if (id.includes('/@tanstack/')) return 'vendor-query'
  if (id.includes('/@radix-ui/')) return 'vendor-radix'
  if (id.includes('/lucide-react/')) return 'vendor-icons'
  if (id.includes('/axios/')) return 'vendor-http'
  if (id.includes('/lodash-es/')) return 'vendor-utils'
  if (
    id.includes('/react/') ||
    id.includes('/react-dom/') ||
    id.includes('/scheduler/') ||
    id.includes('/sonner/') ||
    id.includes('/class-variance-authority/') ||
    id.includes('/tailwind-merge/') ||
    id.includes('/clsx/')
  ) {
    return 'vendor-react'
  }
  return 'vendor-misc'
}

export default defineConfig({
  plugins: [react(), tailwindcss()],
  base: '/ui/',
  resolve: {
    alias: {
      '@': path.resolve(__dirname, './src'),
    },
  },
  server: {
    host: '127.0.0.1',
    port: 9023,
    strictPort: true,
    proxy: {
      '/api': {
        target: apiProxyTarget,
        changeOrigin: true,
      },
    },
  },
  build: {
    outDir: 'dist',
    emptyOutDir: true,
    rollupOptions: {
      output: { manualChunks },
    },
  },
})
