import { defineConfig } from 'vite'
import react from '@vitejs/plugin-react-swc'
import path from 'path'

const apiProxyTarget = process.env.VITE_API_PROXY_TARGET ?? 'http://127.0.0.1:9022'

function manualChunks(id: string) {
  if (!id.includes('node_modules')) {
    return undefined
  }

  if (id.includes('/react/') || id.includes('/react-dom/') || id.includes('/scheduler/')) {
    return 'vendor-react'
  }
  if (id.includes('/@tanstack/')) {
    return 'vendor-query'
  }
  if (id.includes('/@radix-ui/')) {
    return 'vendor-radix'
  }
  if (id.includes('/lucide-react/')) {
    return 'vendor-icons'
  }
  if (id.includes('/axios/')) {
    return 'vendor-http'
  }
  if (
    id.includes('/sonner/') ||
    id.includes('/class-variance-authority/') ||
    id.includes('/tailwind-merge/') ||
    id.includes('/clsx/')
  ) {
    return 'vendor-ui'
  }

  return 'vendor-misc'
}

export default defineConfig({
  plugins: [react()],
  base: '/admin/',
  resolve: {
    alias: {
      '@': path.resolve(__dirname, './src'),
    },
  },
  server: {
    host: '127.0.0.1',
    port: 9025,
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
      output: {
        manualChunks,
      },
    },
  },
})
