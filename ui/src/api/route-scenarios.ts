import type {
  RouteScenario,
  RouteScenarioApplyResponse,
  RouteScenarioContent,
  RouteScenarioItemKey,
  RouteScenarioListResponse,
  RouteScenarioOptions,
  RouteScenarioPreview,
  SaveRouteScenarioRequest,
  SuccessResponse,
} from "@/types/api";
import { api } from "./http";

export async function getRouteScenarios(): Promise<RouteScenarioListResponse> {
  const { data } = await api.get<RouteScenarioListResponse>("/route-scenarios");
  return data;
}

export async function getRouteScenarioOptions(): Promise<RouteScenarioOptions> {
  const { data } = await api.get<RouteScenarioOptions>(
    "/route-scenarios/options",
  );
  return data;
}

export async function getCurrentRouteScenarioContent(): Promise<RouteScenarioContent> {
  const { data } = await api.get<RouteScenarioContent>(
    "/route-scenarios/current",
  );
  return data;
}

export async function createRouteScenario(
  req: SaveRouteScenarioRequest,
): Promise<RouteScenario> {
  const { data } = await api.post<RouteScenario>("/route-scenarios", req);
  return data;
}

export async function updateRouteScenario(
  id: number,
  req: SaveRouteScenarioRequest,
): Promise<RouteScenario> {
  const { data } = await api.put<RouteScenario>(`/route-scenarios/${id}`, req);
  return data;
}

export async function deleteRouteScenario(
  id: number,
): Promise<SuccessResponse> {
  const { data } = await api.delete<SuccessResponse>(`/route-scenarios/${id}`);
  return data;
}

export async function previewRouteScenario(
  id: number,
): Promise<RouteScenarioPreview> {
  const { data } = await api.get<RouteScenarioPreview>(
    `/route-scenarios/${id}/preview`,
  );
  return data;
}

export async function applyRouteScenario(
  id: number,
  only: RouteScenarioItemKey[] = [],
): Promise<RouteScenarioApplyResponse> {
  const { data } = await api.post<RouteScenarioApplyResponse>(
    `/route-scenarios/${id}/apply`,
    { only },
  );
  return data;
}
