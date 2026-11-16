import { getRouteEffective, getRoutes, updateRoutes } from "@/api/routes";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
export function useRoutes() {
  return useQuery({ queryKey: ["routes"], queryFn: getRoutes });
}
export function useRouteEffective(prefix: string, enabled: boolean) {
  return useQuery({
    queryKey: ["route-effective", prefix],
    queryFn: () => getRouteEffective(prefix),
    enabled,
  });
}
export function useUpdateRoutes() {
  const client = useQueryClient();
  return useMutation({
    mutationFn: updateRoutes,
    onSuccess: (routes) => {
      client.setQueryData(["routes"], routes);
      client.invalidateQueries({ queryKey: ["route-effective"] });
      client.invalidateQueries({ queryKey: ["route-scenario-options"] });
    },
  });
}
