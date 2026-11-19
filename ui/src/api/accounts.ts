import type {
  Account,
  AccountQualityHistoryResponse,
  AccountQualityHistoryWindow,
  AccountsListResponse,
  AccountsStatusResponse,
  AccountTestRequest,
  AccountTestResponse,
  CreateAccountRequest,
  DiscoverAccountSupportedModelsRequest,
  SetSupportedModelsRequest,
  SuccessResponse,
  SupportedModelsResponse,
  UpdateAccountRequest,
} from "@/types/api";
import { api } from "./http";

export async function getAccounts(): Promise<AccountsListResponse> {
  const { data } = await api.get<AccountsListResponse>("/accounts");
  return data;
}

export async function getAccountsStatus(): Promise<AccountsStatusResponse> {
  const { data } = await api.get<AccountsStatusResponse>("/accounts/status");
  return data;
}

export async function createAccount(
  req: CreateAccountRequest,
): Promise<Account> {
  const { data } = await api.post<Account>("/accounts", req);
  return data;
}

export async function updateAccount(
  id: number,
  req: UpdateAccountRequest,
): Promise<Account> {
  const { data } = await api.put<Account>(`/accounts/${id}`, req);
  return data;
}

export async function deleteAccount(id: number): Promise<SuccessResponse> {
  const { data } = await api.delete<SuccessResponse>(`/accounts/${id}`);
  return data;
}

export async function setAccountEnabled(
  id: number,
  enabled: boolean,
): Promise<Account> {
  const { data } = await api.post<Account>(`/accounts/${id}/enabled`, {
    enabled,
  });
  return data;
}

export async function clearAccountAutoDisabled(id: number): Promise<Account> {
  const { data } = await api.post<Account>(
    `/accounts/${id}/auto-disabled/clear`,
  );
  return data;
}

export async function clearAccountCooldown(id: number): Promise<Account> {
  const { data } = await api.post<Account>(`/accounts/${id}/cooldown/clear`);
  return data;
}

export async function setAccountSupportedModels(
  id: number,
  req: SetSupportedModelsRequest,
): Promise<SupportedModelsResponse> {
  const { data } = await api.post<SupportedModelsResponse>(
    `/accounts/${id}/supported-models`,
    req,
  );
  return data;
}

export async function syncAccountSupportedModels(
  id: number,
  req: DiscoverAccountSupportedModelsRequest = {},
): Promise<SupportedModelsResponse> {
  const { data } = await api.post<SupportedModelsResponse>(
    `/accounts/${id}/supported-models/sync`,
    req,
  );
  return data;
}

export async function discoverAccountSupportedModels(
  req: DiscoverAccountSupportedModelsRequest,
): Promise<SupportedModelsResponse> {
  const { data } = await api.post<SupportedModelsResponse>(
    "/accounts/supported-models/discover",
    req,
  );
  return data;
}

export async function discoverStoredAccountSupportedModels(
  id: number,
  req: DiscoverAccountSupportedModelsRequest = {},
): Promise<SupportedModelsResponse> {
  const { data } = await api.post<SupportedModelsResponse>(
    `/accounts/${id}/supported-models/discover`,
    req,
  );
  return data;
}

export async function copyAccount(
  id: number,
  req: { name?: string; enabled?: boolean } = {},
): Promise<Account> {
  const { data } = await api.post<Account>(`/accounts/${id}/copy`, req);
  return data;
}

export async function testAccount(
  id: number,
  req?: AccountTestRequest,
): Promise<AccountTestResponse> {
  const { data } = await api.post<AccountTestResponse>(
    `/accounts/${id}/test`,
    req,
  );
  return data;
}

export async function getAccountEffective(
  id: number,
): Promise<import("@/types/routes").AccountEffective> {
  return (await api.get(`/accounts/${id}/effective`)).data;
}
export async function promoteAccountModels(
  id: number,
): Promise<SupportedModelsResponse> {
  return (await api.post(`/accounts/${id}/models/promote`)).data;
}

/** 账号质量分历史（错误率 / 首字 / 耗时 EWMA 快照），按时间升序；窗口给 from/to 或 hours 二选一。 */
export async function getAccountQualityHistory(
  id: number,
  window: AccountQualityHistoryWindow = { hours: 24 },
): Promise<AccountQualityHistoryResponse> {
  const params = "hours" in window ? { hours: window.hours } : { from: window.from, to: window.to };
  const { data } = await api.get<AccountQualityHistoryResponse>(
    `/accounts/${id}/quality-history`,
    { params },
  );
  return data;
}
