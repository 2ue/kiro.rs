import { useEffect, useState } from 'react'
import { LoginPage } from '@/components/LoginPage'
import { Dashboard } from '@/components/Dashboard'
import { storage } from '@/lib/storage'

export default function App() {
  const [loggedIn, setLoggedIn] = useState(false)

  useEffect(() => {
    setLoggedIn(Boolean(storage.getApiKey()))
  }, [])

  return loggedIn ? <Dashboard onLogout={() => setLoggedIn(false)} /> : <LoginPage onLogin={() => setLoggedIn(true)} />
}
