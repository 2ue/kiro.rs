import { api } from "./http";
export interface ConfigPackage {
  formatVersion: number;
  exportedAt: string;
  accounts: unknown[];
  runtimeConfig: Record<string, unknown>;
  routeScenarios: unknown[];
  modelPricing: unknown[];
  manualModels: unknown[];
  requestApiKeys: unknown[];
}
export interface TransferReport {
  dryRun: boolean;
  unchanged: number;
  changes: Array<{
    kind: string;
    naturalKey: string;
    action: "add" | "overwrite" | "delete";
  }>;
}
export async function exportConfig(
  includeSecrets: boolean,
): Promise<ConfigPackage> {
  return (
    await api.get("/config/export", {
      params: { include_secrets: includeSecrets },
    })
  ).data;
}
export async function importConfig(
  bundle: ConfigPackage,
  dryRun: boolean,
): Promise<TransferReport> {
  return (
    await api.post("/config/import", bundle, { params: { dry_run: dryRun } })
  ).data;
}
