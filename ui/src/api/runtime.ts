import type {
  PromptInjectionCatalog,
  PromptInjectionPreviewRequest,
  PromptInjectionPreviewResponse,
  RuntimeConfig,
  SystemVersionResponse,
  UpdateRuntimeConfigRequest,
} from "@/types/api";
import { api } from "./http";

export async function getRuntimeConfig(): Promise<RuntimeConfig> {
  const { data } = await api.get<RuntimeConfig>("/config/runtime");
  return data;
}

export async function getSystemVersion(): Promise<SystemVersionResponse> {
  const { data } = await api.get<SystemVersionResponse>("/system/version");
  return data;
}

export async function getPromptInjectionCatalog(): Promise<PromptInjectionCatalog> {
  const { data } = await api.get<PromptInjectionCatalog>(
    "/prompt-injection/catalog",
  );
  return data;
}

export async function previewPromptInjection(
  req: PromptInjectionPreviewRequest,
): Promise<PromptInjectionPreviewResponse> {
  const { data } = await api.post<PromptInjectionPreviewResponse>(
    "/prompt-injection/preview",
    req,
  );
  return data;
}

export async function updateRuntimeConfig(
  req: UpdateRuntimeConfigRequest,
): Promise<RuntimeConfig> {
  const { data } = await api.put<RuntimeConfig>("/config/runtime", req);
  return data;
}
