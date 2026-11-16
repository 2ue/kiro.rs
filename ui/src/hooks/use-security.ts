import { getAccessKeys, getRequestKeyUsageLimits } from "@/api/security";
import { useQuery } from "@tanstack/react-query";
export function useAccessKeys() {
  return useQuery({ queryKey: ["access-keys"], queryFn: getAccessKeys });
}
export function useRequestKeyUsageLimits() {
  return useQuery({
    queryKey: ["request-key-usage-limits"],
    queryFn: getRequestKeyUsageLimits,
    refetchInterval: 30000,
  });
}
