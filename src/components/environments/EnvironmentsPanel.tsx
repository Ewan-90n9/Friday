import { useEffect, useState } from "react";
import { Globe, Plus, CircleNotch } from "@phosphor-icons/react";
import type { EnvironmentRow } from "@/lib/types";
import { useEnvStore } from "@/store/envStore";
import { EnvironmentListItem } from "./EnvironmentListItem";
import { EnvironmentDialog } from "./EnvironmentDialog";
import { DeleteEnvConfirmDialog } from "./DeleteEnvConfirmDialog";

export function EnvironmentsPanel() {
  const environments = useEnvStore((s) => s.environments);
  const loading = useEnvStore((s) => s.loading);
  const error = useEnvStore((s) => s.error);
  const load = useEnvStore((s) => s.load);
  const remove = useEnvStore((s) => s.remove);

  const [dialogOpen, setDialogOpen] = useState(false);
  const [editing, setEditing] = useState<EnvironmentRow | null>(null);
  const [deleting, setDeleting] = useState<EnvironmentRow | null>(null);

  useEffect(() => {
    load();
  }, [load]);

  return (
    <div className="flex flex-col min-h-0 max-h-[45%] border-b border-border">
      <div className="flex items-center gap-2 h-10 px-4 border-b border-border shrink-0">
        <Globe size={14} className="text-muted-foreground" aria-hidden="true" />
        <span
          className="text-xs font-medium text-muted-foreground uppercase tracking-wide"
          style={{ fontFamily: "var(--font-mono)" }}
        >
          环境
        </span>
        <span className="text-xs text-muted-foreground/60 ml-auto">{environments.length}</span>
        <button
          onClick={() => {
            setEditing(null);
            setDialogOpen(true);
          }}
          aria-label="新增环境"
          className="flex items-center justify-center w-5 h-5 rounded text-accent hover:bg-surface-3 transition-colors cursor-pointer"
        >
          <Plus size={12} aria-hidden="true" />
        </button>
      </div>

      <div className="flex-1 overflow-y-auto px-3 py-3">
        {error && <div className="text-destructive text-xs px-1 py-2">{error}</div>}
        {loading && environments.length === 0 && (
          <div className="flex items-center justify-center gap-2 py-4 text-muted-foreground text-xs">
            <CircleNotch size={14} className="animate-spin" aria-hidden="true" />
            加载中…
          </div>
        )}
        {!loading && environments.length === 0 && (
          <div className="py-4 text-center text-muted-foreground text-xs leading-relaxed">
            暂无环境
            <br />
            点击右上角 + 添加远程诊断环境
          </div>
        )}
        {environments.length > 0 && (
          <ul className="flex flex-col gap-1.5">
            {environments.map((env) => (
              <li key={env.id}>
                <EnvironmentListItem
                  env={env}
                  onEdit={(e) => {
                    setEditing(e);
                    setDialogOpen(true);
                  }}
                  onDelete={(e) => setDeleting(e)}
                />
              </li>
            ))}
          </ul>
        )}
      </div>

      <EnvironmentDialog open={dialogOpen} onClose={() => setDialogOpen(false)} editing={editing} />
      <DeleteEnvConfirmDialog
        env={deleting}
        onConfirm={async () => {
          if (deleting) await remove(deleting.id);
          setDeleting(null);
        }}
        onCancel={() => setDeleting(null)}
      />
    </div>
  );
}
