import {
  getAccountEffective,
  getAccountQualityHistory,
  getAccounts,
  promoteAccountModels,
} from "@/api/accounts";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
export function useAccounts() {
  return useQuery({ queryKey: ["accounts"], queryFn: getAccounts });
}
/** 账号质量分历史；`id` 为空时不查。 */
export function useAccountQualityHistory(id: number | undefined, hours: number) {
  return useQuery({
    queryKey: ["account-quality-history", id, hours],
    queryFn: () => getAccountQualityHistory(id!, hours),
    enabled: id !== undefined && id > 0,
    refetchInterval: 30000,
    placeholderData: (previousData) => previousData,
  });
}
export function useAccountEffective(id: number | null) {
  return useQuery({
    queryKey: ["account-effective", id],
    queryFn: () => getAccountEffective(id!),
    enabled: id !== null,
  });
}
export function usePromoteAccountModels() {
  const client = useQueryClient();
  return useMutation({
    mutationFn: promoteAccountModels,
    onSuccess: () =>
      client.invalidateQueries({ queryKey: ["model-capabilities"] }),
  });
}
