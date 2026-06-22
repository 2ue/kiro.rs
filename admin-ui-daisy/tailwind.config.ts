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
        // Dark gold command-center palette for the default console experience.
        noirGold: {
          'color-scheme': 'dark',
          primary: '#E8C160',
          'primary-content': '#161108',
          secondary: '#B98D3A',
          'secondary-content': '#171106',
          accent: '#F6E3A1',
          'accent-content': '#171106',
          neutral: '#101010',
          'neutral-content': '#F8F2E3',
          'base-100': '#11100E',
          'base-200': '#080808',
          'base-300': '#2A2215',
          'base-content': '#F7EEDB',
          info: '#D6B256',
          'info-content': '#171106',
          success: '#36D399',
          'success-content': '#04130E',
          warning: '#FBBF24',
          'warning-content': '#181004',
          error: '#FB7185',
          'error-content': '#1B070B',
          '--rounded-box': '0.625rem',
          '--rounded-btn': '0.45rem',
          '--rounded-badge': '0.45rem',
          '--animation-btn': '0.15s',
          '--btn-focus-scale': '0.98',
          '--border-btn': '1px',
          '--tab-border': '1px',
          '--tab-radius': '0.5rem',
        },
      },
      {
        // Cyan and violet palette for high-contrast operations rooms.
        auroraCircuit: {
          'color-scheme': 'dark',
          primary: '#22D3EE',
          'primary-content': '#031417',
          secondary: '#A78BFA',
          'secondary-content': '#100B22',
          accent: '#A3E635',
          'accent-content': '#0B1303',
          neutral: '#12131D',
          'neutral-content': '#EFFBFF',
          'base-100': '#12141F',
          'base-200': '#090B12',
          'base-300': '#1E3340',
          'base-content': '#EFFBFF',
          info: '#38BDF8',
          'info-content': '#03121B',
          success: '#34D399',
          'success-content': '#04130E',
          warning: '#FACC15',
          'warning-content': '#151101',
          error: '#F43F5E',
          'error-content': '#1A0509',
          '--rounded-box': '0.625rem',
          '--rounded-btn': '0.45rem',
          '--rounded-badge': '0.45rem',
          '--animation-btn': '0.15s',
          '--btn-focus-scale': '0.98',
          '--border-btn': '1px',
          '--tab-border': '1px',
          '--tab-radius': '0.5rem',
        },
      },
      {
        // Red-gold palette for alert-heavy monitoring without falling back to grayscale.
        emberVault: {
          'color-scheme': 'dark',
          primary: '#FFB020',
          'primary-content': '#170D02',
          secondary: '#FF5C8A',
          'secondary-content': '#1B060D',
          accent: '#2DD4BF',
          'accent-content': '#031412',
          neutral: '#171217',
          'neutral-content': '#FFF1E8',
          'base-100': '#171217',
          'base-200': '#0E0A0D',
          'base-300': '#3A1F24',
          'base-content': '#FFF1E8',
          info: '#60A5FA',
          'info-content': '#06101E',
          success: '#22C55E',
          'success-content': '#031309',
          warning: '#F59E0B',
          'warning-content': '#170D02',
          error: '#FF4D4D',
          'error-content': '#1A0505',
          '--rounded-box': '0.625rem',
          '--rounded-btn': '0.45rem',
          '--rounded-badge': '0.45rem',
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
