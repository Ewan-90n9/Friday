import { PencilSimple, Trash, Key, Password } from "@phosphor-icons/react";
import type { EnvironmentRow } from "@/lib/types";

interface EnvironmentListItemProps {
  env: EnvironmentRow;
  onEdit: (env: EnvironmentRow) => void;
  onDelete: (env: EnvironmentRow) => void;
}

export function EnvironmentListItem({ env, onEdit, onDelete }: EnvironmentListItemProps) {
  return (
    <div className="group px-2.5 py-2 rounded-lg border border-border bg-surface-2/50">
      <div className="flex items-center gap-1.5 mb-1">
        <span
          className="text-xs text-foreground font-medium truncate"
          style={{ fontFamily: "var(--font-mono)" }}
          title={env.name}
        >
          {env.name}
        </span>
        <span
          className="shrink-0 ml-auto flex items-center gap-1 px-1.5 py-px rounded text-[10px] border bg-muted/50 text-muted-foreground border-border"
          title={env.auth_type === "private_key" ? "私钥认证" : "密码认证"}
        >
          {env.auth_type === "private_key" ? (
            <Key size={10} aria-hidden="true" />
          ) : (
            <Password size={10} aria-hidden="true" />
          )}
          {env.auth_type === "private_key" ? "密钥" : "密码"}
        </span>
      </div>
      <div className="flex items-center gap-2">
        <span
          className="text-xs text-muted-foreground truncate flex-1"
          style={{ fontFamily: "var(--font-mono)" }}
          title={`${env.user}@${env.host}:${env.port}`}
        >
          {env.user}@{env.host}:{env.port}
        </span>
        <span className="shrink-0 hidden group-hover:flex items-center gap-1">
          <button
            onClick={() => onEdit(env)}
            aria-label={`编辑 ${env.name}`}
            className="p-1 rounded text-muted-foreground hover:text-foreground hover:bg-surface-3 transition-colors cursor-pointer"
          >
            <PencilSimple size={12} aria-hidden="true" />
          </button>
          <button
            onClick={() => onDelete(env)}
            aria-label={`删除 ${env.name}`}
            className="p-1 rounded text-muted-foreground hover:text-destructive hover:bg-surface-3 transition-colors cursor-pointer"
          >
            <Trash size={12} aria-hidden="true" />
          </button>
        </span>
      </div>
    </div>
  );
}
