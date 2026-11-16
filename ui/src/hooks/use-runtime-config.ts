import {
  getPromptInjectionCatalog,
  getRuntimeConfig,
  getSystemVersion,
  previewPromptInjection,
  updateRuntimeConfig,
} from "@/api/runtime";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
export function useRuntimeConfig() {
  return useQuery({ queryKey: ["runtime-config"], queryFn: getRuntimeConfig });
}
export function useSystemVersion() {
  return useQuery({ queryKey: ["system-version"], queryFn: getSystemVersion });
}
export function usePromptInjectionCatalog() {
  return useQuery({
    queryKey: ["prompt-injection-catalog"],
    queryFn: getPromptInjectionCatalog,
  });
}
export function usePromptInjectionPreview() {
  return useMutation({ mutationFn: previewPromptInjection });
}
export function useUpdateRuntimeConfig() {
  const client = useQueryClient();
  return useMutation({
    mutationFn: updateRuntimeConfig,
    onSuccess: (value) => {
      client.setQueryData(["runtime-config"], value);
      client.invalidateQueries({ queryKey: ["routes"] });
    },
  });
}
