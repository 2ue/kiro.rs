import type {
  ModelCatalogAdoptResponse,
  ModelCatalogDiscoverResponse,
  ManualModelResponse,
  ModelCapabilitiesStatus,
  ModelPricingStatus,
  UpsertManualModelRequest,
} from "@/types/api";
import { api } from "./http";
export async function getModelPricing(): Promise<ModelPricingStatus> {
  const { data } = await api.get<ModelPricingStatus>("/model-pricing");
  return data;
}

export async function syncModelPricing(): Promise<ModelPricingStatus> {
  const { data } = await api.post<ModelPricingStatus>("/model-pricing/sync");
  return data;
}

export async function getModelCapabilities(): Promise<ModelCapabilitiesStatus> {
  const { data } = await api.get<ModelCapabilitiesStatus>(
    "/model-capabilities",
  );
  return data;
}

export async function upsertManualModel(
  payload: UpsertManualModelRequest,
): Promise<ManualModelResponse> {
  const { data } = await api.post<ManualModelResponse>(
    "/model-capabilities/manual",
    payload,
  );
  return data;
}

export async function deleteManualModel(
  model: string,
): Promise<ManualModelResponse> {
  const { data } = await api.delete<ManualModelResponse>(
    `/model-capabilities/manual/${encodeURIComponent(model)}`,
  );
  return data;
}
export async function setModelListed(model: string, listed: boolean) {
  return (
    await api.patch(
      "/model-capabilities/" + encodeURIComponent(model) + "/listed",
      { listed },
    )
  ).data;
}

export async function discoverModelCatalog(): Promise<ModelCatalogDiscoverResponse> {
  const { data } = await api.post<ModelCatalogDiscoverResponse>("/model-capabilities/discover");
  return data;
}
export async function adoptModelCatalog(models: string[], listed = true): Promise<ModelCatalogAdoptResponse> {
  const { data } = await api.post<ModelCatalogAdoptResponse>("/model-capabilities/adopt", { models, listed });
  return data;
}
