import type { Config } from 'tailwindcss'
import daisyui from 'daisyui'

export default {
  content: ['./index.html', './src/**/*.{ts,tsx}'],
  safelist: [
    // Alerts
    'alert', 'alert-error', 'alert-info', 'alert-success', 'alert-warning',
    // Badges
    'badge', 'badge-accent', 'badge-error', 'badge-ghost', 'badge-info',
    'badge-neutral', 'badge-primary', 'badge-secondary', 'badge-sm', 'badge-xs',
    'badge-success', 'badge-warning',
    // Buttons
    'btn', 'btn-accent', 'btn-circle', 'btn-error', 'btn-ghost', 'btn-info',
    'btn-neutral', 'btn-outline', 'btn-primary', 'btn-secondary', 'btn-sm',
    'btn-square', 'btn-success', 'btn-warning', 'btn-xs',
    // Cards
    'card', 'card-bordered', 'card-body',
    // Forms
    'checkbox', 'checkbox-sm', 'checkbox-xs',
    'form-control', 'input', 'input-bordered', 'input-sm', 'input-xs',
    'select', 'select-bordered', 'select-sm', 'select-xs',
    'textarea', 'textarea-bordered', 'textarea-sm', 'textarea-xs',
    'toggle', 'toggle-primary', 'toggle-sm', 'toggle-success', 'toggle-xs',
    // Layout
    'collapse', 'collapse-arrow', 'collapse-content', 'collapse-title',
    'join', 'join-item',
    'modal', 'modal-action', 'modal-backdrop', 'modal-box',
    'navbar', 'navbar-end', 'navbar-start',
    // Feedback
    'loading', 'loading-lg', 'loading-sm', 'loading-spinner', 'loading-xs',
    'progress', 'progress-primary',
    // Data
    'stat', 'stat-desc', 'stat-title', 'stat-value', 'stats',
    'table', 'table-sm', 'table-zebra',
    // Labels
    'label', 'label-text', 'label-text-alt',
    // Menu
    'menu', 'dropdown', 'dropdown-content', 'dropdown-end',
    // Tooltip
    'tooltip', 'tooltip-right',
  ],
  theme: {
    extend: {
      fontFamily: {
        sans: ['Inter', 'ui-sans-serif', 'system-ui', '-apple-system', 'sans-serif'],
        mono: ['JetBrains Mono', 'Menlo', 'Monaco', 'monospace'],
      },
      animation: {
        'fade-in': 'fadeIn 0.2s ease-out',
        'slide-up': 'slideUp 0.2s ease-out',
        'slide-down': 'slideDown 0.2s ease-out',
      },
      keyframes: {
        fadeIn: {
          '0%': { opacity: '0' },
          '100%': { opacity: '1' },
        },
        slideUp: {
          '0%': { opacity: '0', transform: 'translateY(8px)' },
          '100%': { opacity: '1', transform: 'translateY(0)' },
        },
        slideDown: {
          '0%': { opacity: '0', transform: 'translateY(-8px)' },
          '100%': { opacity: '1', transform: 'translateY(0)' },
        },
      },
    },
  },
  plugins: [daisyui],
  daisyui: {
    themes: [
      {
        blackGold: {
          'color-scheme': 'light',
          primary: '#B88A2E',
          'primary-content': '#111111',
          secondary: '#111827',
          'secondary-content': '#FFFFFF',
          accent: '#B88A2E',
          'accent-content': '#111111',
          neutral: '#111827',
          'neutral-content': '#F9FAFB',
          'base-100': '#FFFFFF',
          'base-200': '#F3F4F6',
          'base-300': '#E5E7EB',
          'base-content': '#111827',
          info: '#2563EB',
          'info-content': '#EFF6FF',
          success: '#16A34A',
          'success-content': '#ECFDF3',
          warning: '#D97706',
          'warning-content': '#FFFBEB',
          error: '#DC2626',
          'error-content': '#FEF2F2',
          '--rounded-box': '0.5rem',
          '--rounded-btn': '0.375rem',
          '--rounded-badge': '0.35rem',
          '--animation-btn': '0.12s',
          '--btn-focus-scale': '0.99',
          '--border-btn': '1px',
          '--tab-border': '1px',
          '--tab-radius': '0.375rem',
        },
      },
    ],
  },
} satisfies Config
