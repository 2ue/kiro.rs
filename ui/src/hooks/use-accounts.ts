import {
  getAccountEffective,
  getAccountQualityHistory,
  getAccounts,
  promoteAccountModels,
} from "@/api/accounts";
import type { AccountQualityHistoryWindow } from "@/types/api";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
export function useAccounts() {
  return useQuery({ queryKey: ["accounts"], queryFn: getAccounts });
}
/**
 * 账号质量分历史；`id` 为空时不查。按固定的 from/to 窗口查时窗口不会变，不需要自动刷新；
 * 按最近 hours 小时查时每 30 秒刷新一次。
 */
export function useAccountQualityHistory(id: number | undefined, window: AccountQualityHistoryWindow) {
  return useQuery({
    queryKey: ["account-quality-history", id, window],
    queryFn: () => getAccountQualityHistory(id!, window),
    enabled: id !== undefined && id > 0,
    refetchInterval: "hours" in window ? 30000 : false,
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
