import { create } from 'zustand'
import { persist } from 'zustand/middleware'

interface PreferencesState {
  theme: 'light' | 'dark'
  pageSize: number
  setTheme: (theme: 'light' | 'dark') => void
  setPageSize: (size: number) => void
}

export const usePreferences = create<PreferencesState>()(
  persist(
    (set) => ({
      theme: 'light',
      pageSize: 20,
      setTheme: (theme) => {
        document.documentElement.classList.toggle('dark', theme === 'dark')
        set({ theme })
      },
      setPageSize: (pageSize) => set({ pageSize }),
    }),
    {
      name: 'kiro-rs.preferences',
      onRehydrateStorage: () => (state) => {
        if (state) {
          document.documentElement.classList.toggle('dark', state.theme === 'dark')
        }
      },
    },
  ),
)
