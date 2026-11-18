import {
  adoptModelCatalog,
  deleteManualModel,
  discoverModelCatalog,
  getModelCapabilities,
  getModelPricing,
  setModelListed,
  syncModelPricing,
  upsertManualModel,
} from "@/api/models";
import type { UpsertManualModelRequest } from "@/types/models";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
export function useModelPricing(refetchInterval: number | false = 60000) {
  return useQuery({
    queryKey: ["model-pricing"],
    queryFn: getModelPricing,
    refetchInterval,
  });
}

export function useSyncModelPricing() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: syncModelPricing,
    onSuccess: () =>
      queryClient.invalidateQueries({ queryKey: ["model-pricing"] }),
  });
}

export function useModelCapabilities() {
  return useQuery({
    queryKey: ["model-capabilities"],
    queryFn: getModelCapabilities,
    refetchInterval: 60000,
  });
}

export function useUpsertManualModel() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (payload: UpsertManualModelRequest) =>
      upsertManualModel(payload),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ["model-capabilities"] });
      queryClient.invalidateQueries({ queryKey: ["model-pricing"] });
    },
  });
}

export function useDeleteManualModel() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: deleteManualModel,
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ["model-capabilities"] });
      queryClient.invalidateQueries({ queryKey: ["model-pricing"] });
    },
  });
}
export function useSetModelListed() {
  const client = useQueryClient();
  return useMutation({
    mutationFn: ({ model, listed }: { model: string; listed: boolean }) =>
      setModelListed(model, listed),
    onSuccess: () =>
      client.invalidateQueries({ queryKey: ["model-capabilities"] }),
  });
}
export function useDiscoverModelCatalog() {
  return useMutation({ mutationFn: discoverModelCatalog });
}
export function useAdoptModelCatalog() {
  const client = useQueryClient();
  return useMutation({
    mutationFn: ({ models, listed }: { models: string[]; listed?: boolean }) => adoptModelCatalog(models, listed),
    onSuccess: () => {
      client.invalidateQueries({ queryKey: ["model-capabilities"] });
      client.invalidateQueries({ queryKey: ["model-pricing"] });
    },
  });
}
