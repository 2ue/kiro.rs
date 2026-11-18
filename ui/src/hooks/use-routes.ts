import { getRouteEffective, getRoutes, updateRoutes } from "@/api/routes";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
export function useRoutes() {
  return useQuery({ queryKey: ["routes"], queryFn: getRoutes });
}
export function useRouteEffective(id: number, enabled: boolean) {
  return useQuery({
    queryKey: ["route-effective", id],
    queryFn: () => getRouteEffective(id),
    enabled: enabled && id > 0,
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
