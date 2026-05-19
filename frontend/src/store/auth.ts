import { create } from 'zustand'
import { storage } from '@/lib/storage'

interface AuthState {
  apiKey: string | null
  isAuthed: boolean
  login: (key: string) => void
  logout: () => void
}

export const useAuth = create<AuthState>((set) => ({
  apiKey: storage.getApiKey(),
  isAuthed: Boolean(storage.getApiKey()),
  login: (key: string) => {
    storage.setApiKey(key)
    set({ apiKey: key, isAuthed: true })
  },
  logout: () => {
    storage.removeApiKey()
    set({ apiKey: null, isAuthed: false })
  },
}))
