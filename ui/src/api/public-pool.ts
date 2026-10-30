import axios from 'axios'
import type { PublicPoolQuery, PublicPoolResponse } from '@/types/api'

/**
 * The public read-only pool page. A plain request: it never sends the admin key, and a 404
 * (page closed) must not log the viewer out of anything.
 */
export async function getPublicPool(query: PublicPoolQuery): Promise<PublicPoolResponse> {
  const { data } = await axios.get<PublicPoolResponse>('/api/public/pool', { params: query })
  return data
}
