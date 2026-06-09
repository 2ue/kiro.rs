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
        // Kiro official palette: purple brand system with warm lavender surfaces.
        kiroOfficial: {
          primary: '#9046FF',
          'primary-content': '#FFFFFF',
          secondary: '#B98CFF',
          'secondary-content': '#211D25',
          accent: '#C2A0FD',
          'accent-content': '#211D25',
          neutral: '#19161D',
          'neutral-content': '#FAF8FF',
          'base-100': '#FFFFFF',
          'base-200': '#F4EDFF',
          'base-300': '#DCCBFF',
          'base-content': '#19161D',
          info: '#69A0F6',
          'info-content': '#FFFFFF',
          success: '#28A966',
          'success-content': '#FFFFFF',
          warning: '#C89600',
          'warning-content': '#19161D',
          error: '#DE112C',
          'error-content': '#FFFFFF',
          '--rounded-box': '0.75rem',
          '--rounded-btn': '0.5rem',
          '--rounded-badge': '0.5rem',
          '--animation-btn': '0.15s',
          '--btn-focus-scale': '0.98',
          '--border-btn': '1px',
          '--tab-border': '1px',
          '--tab-radius': '0.5rem',
        },
      },
      {
        // Softer lavender variant: same semantics, lower saturation for dense screens.
        kiroLavender: {
          primary: '#7C3AED',
          'primary-content': '#FFFFFF',
          secondary: '#A78BFA',
          'secondary-content': '#211D25',
          accent: '#C4B5FD',
          'accent-content': '#211D25',
          neutral: '#19161D',
          'neutral-content': '#FAF8FF',
          'base-100': '#FFFFFF',
          'base-200': '#F6F0FF',
          'base-300': '#DECEFF',
          'base-content': '#19161D',
          info: '#69A0F6',
          'info-content': '#FFFFFF',
          success: '#28A966',
          'success-content': '#FFFFFF',
          warning: '#C89600',
          'warning-content': '#19161D',
          error: '#DE112C',
          'error-content': '#FFFFFF',
          '--rounded-box': '0.75rem',
          '--rounded-btn': '0.5rem',
          '--rounded-badge': '0.5rem',
          '--animation-btn': '0.15s',
          '--btn-focus-scale': '0.98',
          '--border-btn': '1px',
          '--tab-border': '1px',
          '--tab-radius': '0.5rem',
        },
      },
      {
        // Focus variant: stronger purple contrast without changing page surfaces into a skin.
        kiroFocus: {
          primary: '#6D28D9',
          'primary-content': '#FFFFFF',
          secondary: '#8B5CF6',
          'secondary-content': '#FFFFFF',
          accent: '#B98CFF',
          'accent-content': '#211D25',
          neutral: '#19161D',
          'neutral-content': '#FAF8FF',
          'base-100': '#FFFFFF',
          'base-200': '#F3ECFF',
          'base-300': '#D6C2FF',
          'base-content': '#19161D',
          info: '#69A0F6',
          'info-content': '#FFFFFF',
          success: '#28A966',
          'success-content': '#FFFFFF',
          warning: '#C89600',
          'warning-content': '#19161D',
          error: '#DE112C',
          'error-content': '#FFFFFF',
          '--rounded-box': '0.75rem',
          '--rounded-btn': '0.5rem',
          '--rounded-badge': '0.5rem',
          '--animation-btn': '0.15s',
          '--btn-focus-scale': '0.98',
          '--border-btn': '1px',
          '--tab-border': '1px',
          '--tab-radius': '0.5rem',
        },
      },
    ],
  },
} satisfies Config
