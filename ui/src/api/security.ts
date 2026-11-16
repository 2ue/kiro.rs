import type {
  AccessKeysResponse,
  CreateRequestApiKeyRequest,
  KeyLimitPeriod,
  RequestKeyUsageLimitsResponse,
  UpdateAdminApiKeyRequest,
  UpdateRequestApiKeyRequest,
} from "@/types/api";
import { api } from "./http";

export async function getAccessKeys(): Promise<AccessKeysResponse> {
  const { data } = await api.get<AccessKeysResponse>("/security/keys");
  return data;
}

export async function updateAdminApiKey(
  req: UpdateAdminApiKeyRequest,
): Promise<AccessKeysResponse> {
  const { data } = await api.put<AccessKeysResponse>(
    "/security/admin-key",
    req,
  );
  return data;
}

export async function createRequestApiKey(
  req: CreateRequestApiKeyRequest = {},
): Promise<AccessKeysResponse> {
  const { data } = await api.post<AccessKeysResponse>(
    "/security/request-keys",
    req,
  );
  return data;
}

export async function updateRequestApiKey(
  id: string,
  req: UpdateRequestApiKeyRequest,
): Promise<AccessKeysResponse> {
  const { data } = await api.put<AccessKeysResponse>(
    `/security/request-keys/${id}`,
    req,
  );
  return data;
}

export async function getRequestKeyUsageLimits(): Promise<RequestKeyUsageLimitsResponse> {
  const { data } = await api.get<RequestKeyUsageLimitsResponse>(
    "/security/request-keys/usage-limits",
  );
  return data;
}

export async function resetRequestKeyUsage(
  id: string,
  period: KeyLimitPeriod,
): Promise<void> {
  await api.post(`/security/request-keys/${id}/usage-limits/reset`, { period });
}

export async function deleteRequestApiKey(
  id: string,
): Promise<AccessKeysResponse> {
  const { data } = await api.delete<AccessKeysResponse>(
    `/security/request-keys/${id}`,
  );
  return data;
}
