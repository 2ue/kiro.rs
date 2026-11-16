import {
  getAccountEffective,
  getAccounts,
  promoteAccountModels,
} from "@/api/accounts";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
export function useAccounts() {
  return useQuery({ queryKey: ["accounts"], queryFn: getAccounts });
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
