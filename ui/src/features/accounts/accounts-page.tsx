import {
  clearAccountAutoDisabled,
  clearAccountCooldown,
  createAccount,
  deleteAccount,
  discoverAccountSupportedModels,
  discoverStoredAccountSupportedModels,
  getAccountsStatus,
  setAccountEnabled,
  updateAccount,
} from "@/api/accounts";
import {
  EmptyState,
  ErrorState,
  LoadingState,
  MiddleTruncate,
  PageContainer,
  PageHeader,
  useConfirm,
} from "@/components/patterns";
import {
  Button,
  Input,
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
  Switch,
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui";
import { EffectiveSheet } from "@/features/routes/effective-sheet";
import {
  useAccountEffective,
  useAccounts,
  usePromoteAccountModels,
} from "@/hooks/use-accounts";
import { formatDate } from "@/lib/format";
import { extractErrorMessage } from "@/lib/utils";
import type { Account, CreateAccountRequest } from "@/types/api";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Eye, Pencil, Play, Plus, Trash2 } from "lucide-react";
import { useState } from "react";
import { toast } from "sonner";
import { AccountFormModal } from "./account-form-modal";
import { AccountTestModal } from "./account-test-modal";
import {
  defaultAccountForm,
  parseHeaderOverridesText,
  parseModelMappingRules,
  parseSupportedModelsText,
  accountFormFromAccount,
  type AccountFormDraft,
} from "./account-utils";
export function AccountsPage() {
  const query = useAccounts(),
    client = useQueryClient(),
    confirm = useConfirm(),
    promote = usePromoteAccountModels();
  const statuses = useQuery({
    queryKey: ["accounts-status"],
    queryFn: getAccountsStatus,
    refetchInterval: 15000,
  });
  const [filter, setFilter] = useState(""),
    [tag, setTag] = useState("_all"),
    [editor, setEditor] = useState<{
      account: Account | null;
      draft: AccountFormDraft;
    } | null>(null),
    [testing, setTesting] = useState<Account | null>(null),
    [effectiveId, setEffectiveId] = useState<number | null>(null);
  const effective = useAccountEffective(effectiveId);
  const mutation = useMutation({
    mutationFn: async (action: () => Promise<unknown>) => action(),
    onSuccess: () => {
      client.invalidateQueries({ queryKey: ["accounts"] });
      client.invalidateQueries({ queryKey: ["accounts-status"] });
      client.invalidateQueries({ queryKey: ["account-effective"] });
    },
    onError: (error) => toast.error(extractErrorMessage(error)),
  });
  if (query.isLoading) return <LoadingState />;
  if (query.error) return <ErrorState title="读取账号失败" />;
  const accounts = query.data?.accounts ?? [],
    tags = [...new Set(accounts.flatMap((a) => a.tags ?? []))].sort(),
    visible = accounts.filter(
      (a) =>
        (!filter ||
          `${a.name} ${a.id}`.toLowerCase().includes(filter.toLowerCase())) &&
        (tag === "_all" || a.tags?.includes(tag)),
    );
  const save = () => {
    if (!editor) return;
    mutation.mutate(async () => {
      const {
        tagsText,
        modelMappingRulesText,
        supportedModelsText,
        headerOverridesText,
        streamResponseMode,
        proxyUrl,
        ...form
      } = editor.draft;
      const request: CreateAccountRequest = {
        ...form,
        tags: tagsText
          .split(/[,，]/)
          .map((t) => t.trim())
          .filter(Boolean),
        proxyUrl: proxyUrl || null,
        headerOverrides: parseHeaderOverridesText(headerOverridesText),
        modelMappingRules: parseModelMappingRules(modelMappingRulesText),
        supportedModels: parseSupportedModelsText(supportedModelsText),
        streamResponseMode:
          streamResponseMode === "inherit" ? null : streamResponseMode,
      };
      if (editor.account) {
        const { apiKey, proxyUrl: nextProxy, ...rest } = request;
        await updateAccount(editor.account.id, {
          ...rest,
          ...(proxyUrl !== (editor.account.proxyUrl ?? "")
            ? { proxyUrl: nextProxy }
            : {}),
          ...(apiKey ? { apiKey } : {}),
        });
      } else await createAccount(request);
      setEditor(null);
      toast.success("账号已保存");
    });
  };
  return (
    <PageContainer>
      <PageHeader
        actions={
          <Button
            onClick={() =>
              setEditor({ account: null, draft: defaultAccountForm() })
            }
          >
            <Plus />
            新增账号
          </Button>
        }
      />
      <div className="flex flex-wrap gap-3">
        <Input
          className="max-w-sm"
          placeholder="搜索名称或 id"
          value={filter}
          onChange={(e) => setFilter(e.target.value)}
        />
        <Select value={tag} onValueChange={setTag}>
          <SelectTrigger className="w-52">
            <SelectValue />
          </SelectTrigger>
          <SelectContent>
            <SelectItem value="_all">全部标签</SelectItem>
            {tags.map((tag) => (
              <SelectItem value={tag} key={tag}>
                {tag}
              </SelectItem>
            ))}
          </SelectContent>
        </Select>
      </div>
      {visible.length === 0 ? (
        <EmptyState title="暂无匹配账号" />
      ) : (
        <Table>
          <TableHeader>
            <TableRow>
              {[
                "账号",
                "标签",
                "启用",
                "状态",
                "并发",
                "成本倍率",
                "更新",
                "操作",
              ].map((t) => (
                <TableHead key={t}>{t}</TableHead>
              ))}
            </TableRow>
          </TableHeader>
          <TableBody>
            {visible.map((a) => {
              const status = statuses.data?.accounts.find(
                (s) => s.account.id === a.id,
              );
              return (
                <TableRow key={a.id}>
                  <TableCell>
                    <div className="font-medium">
                      <MiddleTruncate text={a.name} className="max-w-56"/>
                      <span className="text-xs text-muted-foreground">
                        #{a.id}
                      </span>
                    </div>
                    <div className="text-xs text-muted-foreground">
                      {a.baseUrl}
                    </div>
                  </TableCell>
                  <TableCell>{a.tags?.join("、") || "—"}</TableCell>
                  <TableCell>
                    <Switch
                      checked={a.enabled}
                      disabled={mutation.isPending}
                      onCheckedChange={(v) =>
                        mutation.mutate(() => setAccountEnabled(a.id, v))
                      }
                    />
                  </TableCell>
                  <TableCell>
                    {a.autoDisabled
                      ? "自动禁用"
                      : status?.dispatchable
                        ? "可调度"
                        : (status?.skippedReason ?? "—")}
                  </TableCell>
                  <TableCell>
                    {status?.inFlight ?? 0} / {a.maxConcurrentRequests}
                  </TableCell>
                  <TableCell>{a.costMultiplier}</TableCell>
                  <TableCell>{formatDate(a.updatedAt)}</TableCell>
                  <TableCell>
                    <div className="flex flex-wrap gap-1">
                      <Button
                        variant="ghost"
                        size="sm"
                        onClick={() =>
                          setEditor({ account: a, draft: accountFormFromAccount(a) })
                        }
                      >
                        <Pencil />
                        编辑
                      </Button>
                      <Button
                        variant="ghost"
                        size="sm"
                        onClick={() => setEffectiveId(a.id)}
                      >
                        <Eye />
                        生效参数
                      </Button>
                      <Button
                        variant="ghost"
                        size="sm"
                        onClick={() => setTesting(a)}
                      >
                        <Play />
                        测试
                      </Button>
                      <Button
                        variant="ghost"
                        size="sm"
                        disabled={mutation.isPending}
                        onClick={() =>
                          mutation.mutate(async () => {
                            const found =
                              await discoverStoredAccountSupportedModels(a.id);
                            await updateAccount(a.id, {
                              supportedModels: found.supportedModels,
                            });
                            toast.success(`发现 ${found.count} 个模型`);
                          })
                        }
                      >
                        发现模型
                      </Button>
                      <Button
                        variant="ghost"
                        size="sm"
                        disabled={promote.isPending}
                        onClick={async () => {
                          try {
                            const value = await promote.mutateAsync(a.id);
                            toast.success(`已加入目录 ${value.count} 个模型`);
                          } catch (error) {
                            toast.error(extractErrorMessage(error));
                          }
                        }}
                      >
                        加入目录
                      </Button>
                      <Button
                        variant="ghost"
                        size="sm"
                        onClick={() =>
                          mutation.mutate(() => clearAccountCooldown(a.id))
                        }
                      >
                        清冷却
                      </Button>
                      <Button
                        variant="ghost"
                        size="sm"
                        onClick={() =>
                          mutation.mutate(() => clearAccountAutoDisabled(a.id))
                        }
                      >
                        清自动禁用
                      </Button>
                      <Button
                        variant="ghost"
                        size="sm"
                        onClick={async () => {
                          if (
                            await confirm({
                              title: "删除账号",
                              message: a.name,
                              confirmText: "删除",
                              tone: "danger",
                            })
                          )
                            mutation.mutate(() => deleteAccount(a.id));
                        }}
                      >
                        <Trash2 />
                      </Button>
                    </div>
                  </TableCell>
                </TableRow>
              );
            })}
          </TableBody>
        </Table>
      )}
      {editor && (
        <AccountFormModal
          mode={editor.account ? "edit" : "create"}
          account={editor.account}
          open
          draft={editor.draft}
          saving={mutation.isPending}
          onDraftChange={(v) =>
            setEditor({
              ...editor,
              draft: typeof v === "function" ? v(editor.draft) : v,
            })
          }
          onClose={() => setEditor(null)}
          onSubmit={save}
          onDiscoverSupportedModels={async () => {
            try {
              const result =
                editor.account && !editor.draft.apiKey
                  ? await discoverStoredAccountSupportedModels(
                      editor.account.id,
                    )
                  : await discoverAccountSupportedModels({
                      baseUrl: editor.draft.baseUrl,
                      apiKey: editor.draft.apiKey || undefined,
                      authType: editor.draft.authType,
                    });
              return result.supportedModels;
            } catch (error) {
              toast.error(extractErrorMessage(error));
              throw error;
            }
          }}
        />
      )}
      <AccountTestModal
        account={testing}
        open={testing !== null}
        onClose={() => setTesting(null)}
        onDone={() =>
          client.invalidateQueries({ queryKey: ["accounts-status"] })
        }
      />
      <EffectiveSheet
        open={effectiveId !== null}
        onOpenChange={(v) => {
          if (!v) setEffectiveId(null);
        }}
        title={`账号 #${effectiveId} 生效参数`}
        value={effective.data?.parameters}
        sources={effective.data?.sources}
        loading={effective.isLoading}
        error={effective.error}
      />
    </PageContainer>
  );
}
