import { useCallback, useEffect, useState } from "react";
import { Trash, CircleNotch, FileCode } from "@phosphor-icons/react";
import { listRepoCache, listServiceRepos, deleteRepoCache, deleteServiceRepo } from "@/lib/ipc";
import type { ServiceRepoRow, RepoCacheEntry } from "@/lib/types";

function formatBytes(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 ** 2) return `${(n / 1024).toFixed(1)} KB`;
  if (n < 1024 ** 3) return `${(n / 1024 ** 2).toFixed(1)} MB`;
  return `${(n / 1024 ** 3).toFixed(2)} GB`;
}

export function CodeRepoSection() {
  const [mappings, setMappings] = useState<ServiceRepoRow[] | null>(null);
  const [cache, setCache] = useState<RepoCacheEntry[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState<string | null>(null);

  const refresh = useCallback(async () => {
    try {
      const [m, c] = await Promise.all([listServiceRepos(), listRepoCache()]);
      setMappings(m);
      setCache(c);
      setError(null);
    } catch (e) {
      setError(String(e));
    }
  }, []);

  useEffect(() => {
    refresh();
  }, [refresh]);

  const handleDeleteMapping = async (service: string) => {
    setBusy(`map:${service}`);
    try {
      await deleteServiceRepo(service);
      await refresh();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(null);
    }
  };

  const handleDeleteCache = async (urlHash: string) => {
    setBusy(`cache:${urlHash}`);
    try {
      await deleteRepoCache(urlHash);
      await refresh();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(null);
    }
  };

  return (
    <div className="px-5 py-3 space-y-3">
      <label className="text-sm text-foreground">代码仓</label>
      <p className="text-xs text-muted-foreground">
        agent 诊断时问询的服务→代码仓记忆；缓存为完整 clone（本机 repos 目录），删除后下次 clone 重建
      </p>

      {/* 服务映射 */}
      <div className="space-y-1">
        <p className="text-xs text-muted-foreground">服务映射（{mappings?.length ?? 0}）</p>
        {mappings?.length === 0 && (
          <p className="text-xs text-muted-foreground/70">暂无记忆（agent 问询仓地址后自动记录）</p>
        )}
        {mappings?.map((m) => (
          <div
            key={m.service}
            className="flex items-center gap-2 px-2 py-1.5 rounded-md border border-border bg-surface-2"
          >
            <div className="flex-1 min-w-0">
              <p className="text-xs text-foreground truncate">{m.service}</p>
              <p
                className="text-[11px] text-muted-foreground truncate"
                style={{ fontFamily: "var(--font-mono)" }}
              >
                {m.repo_url}
                {m.last_ref ? ` @ ${m.last_ref}` : ""}
              </p>
            </div>
            <button
              onClick={() => handleDeleteMapping(m.service)}
              disabled={busy === `map:${m.service}`}
              aria-label={`删除 ${m.service} 映射`}
              className="shrink-0 text-muted-foreground hover:text-destructive transition-colors cursor-pointer disabled:opacity-50"
            >
              {busy === `map:${m.service}` ? (
                <CircleNotch size={14} className="animate-spin" aria-hidden="true" />
              ) : (
                <Trash size={14} weight="regular" aria-hidden="true" />
              )}
            </button>
          </div>
        ))}
      </div>

      {/* 缓存仓库 */}
      <div className="space-y-1">
        <p className="text-xs text-muted-foreground">缓存仓库（{cache?.length ?? 0}）</p>
        {cache?.length === 0 && (
          <p className="text-xs text-muted-foreground/70 flex items-center gap-1">
            <FileCode size={12} aria-hidden="true" /> 暂无缓存
          </p>
        )}
        {cache?.map((c) => (
          <div
            key={c.url_hash}
            className="flex items-center gap-2 px-2 py-1.5 rounded-md border border-border bg-surface-2"
          >
            <div className="flex-1 min-w-0">
              <p
                className="text-[11px] text-foreground truncate"
                style={{ fontFamily: "var(--font-mono)" }}
              >
                {c.repo_url}
              </p>
              <p className="text-[11px] text-muted-foreground">
                {formatBytes(c.disk_bytes)} · {c.worktrees} 个检出
              </p>
            </div>
            <button
              onClick={() => handleDeleteCache(c.url_hash)}
              disabled={busy === `cache:${c.url_hash}`}
              aria-label="删除缓存仓库"
              className="shrink-0 text-muted-foreground hover:text-destructive transition-colors cursor-pointer disabled:opacity-50"
            >
              {busy === `cache:${c.url_hash}` ? (
                <CircleNotch size={14} className="animate-spin" aria-hidden="true" />
              ) : (
                <Trash size={14} weight="regular" aria-hidden="true" />
              )}
            </button>
          </div>
        ))}
      </div>

      {error && <p className="text-xs text-destructive break-words">{error}</p>}
    </div>
  );
}
