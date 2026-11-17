export function normalizeRoutePrefix(prefix: string): string {
  const value = prefix.trim().replace(/^\/+|\/+$/g, "");
  return !value || value === "v1" ? "" : `/${value}`;
}
export function displayRoutePrefix(prefix: string): string {
  return normalizeRoutePrefix(prefix) || "/";
}
export function routeEditorPath(prefix: string): string {
  return `/routes/${prefix ? encodeURIComponent(prefix) : "_root"}`;
}
export function isBuiltinRoute(prefix: string): boolean {
  return ["", "/cc", "/ha", "/na"].includes(normalizeRoutePrefix(prefix));
}
export function sortRoutes<T extends { prefix: string }>(routes: T[]): T[] {
  const order = ["", "/cc", "/ha", "/na"];
  return [...routes].sort((a, b) => {
    const ap = normalizeRoutePrefix(a.prefix),
      bp = normalizeRoutePrefix(b.prefix);
    const ai = order.indexOf(ap),
      bi = order.indexOf(bp);
    return ai >= 0 || bi >= 0
      ? (ai < 0 ? 99 : ai) - (bi < 0 ? 99 : bi)
      : ap.localeCompare(bp);
  });
}
export function ruleSummary(
  mode: "allow_all" | "allow_list" | "deny_list",
  rules: string[],
): string {
  if (mode === "allow_all") return "全部";
  if (mode === "allow_list")
    return rules.length ? `仅允许 ${rules.length} 项` : "不允许任何项";
  return rules.length ? `排除 ${rules.length} 项` : "全部";
}

export function accountAllowsRoute(
  account: {
    routeMode: "allow_all" | "allow_list" | "deny_list";
    routeRules: string[];
  },
  prefix: string,
): boolean {
  const endpoint = `${normalizeRoutePrefix(prefix)}/v1/messages`.toLowerCase();
  const matched = (account.routeRules ?? []).some((value) => {
    const rule = value.trim().toLowerCase().replace(/\/+$/, "") || "/";
    return (
      rule === "*" ||
      rule === "/" ||
      endpoint === rule ||
      endpoint.startsWith(`${rule}/`)
    );
  });
  return (
    account.routeMode === "allow_all" ||
    (account.routeMode === "allow_list" ? matched : !matched)
  );
}
