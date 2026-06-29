import { defineConfig } from 'vite'
import react from '@vitejs/plugin-react-swc'
import path from 'path'
import { fileURLToPath } from 'url'

const __dirname = path.dirname(fileURLToPath(import.meta.url))

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
  if (id.includes('/react-daisyui/') || id.includes('/daisyui/')) {
    return 'vendor-daisy'
  }
  if (id.includes('/lucide-react/')) {
    return 'vendor-icons'
  }
  if (id.includes('/axios/')) {
    return 'vendor-http'
  }
  if (id.includes('/lodash-es/')) {
    return 'vendor-utils'
  }
  if (id.includes('/sonner/') || id.includes('/tailwind-merge/') || id.includes('/clsx/')) {
    return 'vendor-ui'
  }

  return 'vendor-misc'
}

export default defineConfig({
  plugins: [react()],
  base: '/console/',
  resolve: {
    alias: {
      '@': path.resolve(__dirname, './src'),
    },
  },
  server: {
    host: '127.0.0.1',
    port: 9024,
    strictPort: true,
    proxy: {
      '/api': {
        target: 'http://127.0.0.1:9022',
        changeOrigin: true,
      },
    },
  },
  build: {
    rollupOptions: {
      output: {
        manualChunks,
      },
    },
  },
})
