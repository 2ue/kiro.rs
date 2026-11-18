import type { RouteConfig, RouteEffective } from "@/types/routes";
import { api } from "./http";
export async function getRoutes(): Promise<RouteConfig[]> {
  return (await api.get("/routes")).data;
}
export async function updateRoutes(
  routes: RouteConfig[],
): Promise<RouteConfig[]> {
  return (await api.put("/routes", routes)).data;
}
export async function getRouteEffective(id: number): Promise<RouteEffective> {
  return (await api.get("/routes/effective", { params: { id } })).data;
}
