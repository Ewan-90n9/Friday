import { CheckCircle, XCircle, Spinner, ArrowDown, ArrowUp, Warning } from "@phosphor-icons/react";
import type { TransferInfo } from "@/lib/types";

interface TransferProgressCardProps {
  transfer: TransferInfo;
}

function formatBytes(bytes: number): string {
  if (bytes === 0) return "0 B";
  const units = ["B", "KB", "MB", "GB", "TB"];
  const i = Math.min(Math.floor(Math.log(bytes) / Math.log(1024)), units.length - 1);
  return `${(bytes / Math.pow(1024, i)).toFixed(1)} ${units[i]}`;
}

export function TransferProgressCard({ transfer }: TransferProgressCardProps) {
  const isDownload = transfer.direction === "download";
  const pct =
    transfer.total_bytes > 0
      ? Math.min(100, (transfer.transferred_bytes / transfer.total_bytes) * 100)
      : 0;
  const isTerminal = ["completed", "failed", "cancelled"].includes(transfer.status);

  const statusLabel = (() => {
    switch (transfer.status) {
      case "pending":
      case "connecting":
        return "连接中...";
      case "transferring":
        return `${formatBytes(transfer.transferred_bytes)} / ${formatBytes(transfer.total_bytes)} · ${formatBytes(transfer.speed_bps)}/s`;
      case "retrying":
        return `重试中（第 ${transfer.attempt} 次）`;
      case "completed":
        return `完成 · ${formatBytes(transfer.total_bytes)}`;
      case "failed":
        return "失败";
      case "cancelled":
        return "已取消";
    }
  })();

  const statusColor = (() => {
    switch (transfer.status) {
      case "completed":
        return "text-success";
      case "failed":
        return "text-destructive";
      case "cancelled":
        return "text-muted-foreground";
      default:
        return "text-accent";
    }
  })();

  return (
    <div className="bg-card border border-border rounded-lg overflow-hidden mb-3">
      <div className="flex items-center gap-2 px-3 py-2">
        {isDownload ? (
          <ArrowDown size={12} weight="bold" className="text-muted-foreground shrink-0" aria-hidden="true" />
        ) : (
          <ArrowUp size={12} weight="bold" className="text-muted-foreground shrink-0" aria-hidden="true" />
        )}
        <span
          className="text-xs font-semibold text-accent bg-accent/10 px-1.5 py-0.5 rounded shrink-0"
          style={{ fontFamily: "var(--font-mono)" }}
        >
          {isDownload ? "下载" : "上传"}
        </span>
        <span
          className="text-xs text-foreground truncate flex-1"
          style={{ fontFamily: "var(--font-mono)" }}
          title={transfer.file_name}
        >
          {transfer.file_name}
        </span>
        <span
          className={`text-xs shrink-0 flex items-center gap-1 ${statusColor}`}
          style={{ fontFamily: "var(--font-mono)" }}
        >
          {!isTerminal && transfer.status !== "retrying" && (
            <Spinner size={12} className="animate-spin" aria-hidden="true" />
          )}
          {transfer.status === "retrying" && (
            <Warning size={12} weight="fill" aria-hidden="true" />
          )}
          {transfer.status === "completed" && (
            <CheckCircle size={12} weight="fill" aria-hidden="true" />
          )}
          {transfer.status === "failed" && (
            <XCircle size={12} weight="fill" aria-hidden="true" />
          )}
          {statusLabel}
        </span>
      </div>
      {/* 进度条：未知大小(total=0)或终态失败时不显示 */}
      {(transfer.total_bytes > 0 || transfer.status === "completed") && transfer.status !== "failed" && (
        <div className="px-3 pb-2">
          <div className="h-1 bg-surface-2 rounded-full overflow-hidden" role="progressbar" aria-valuenow={Math.round(pct)} aria-valuemin={0} aria-valuemax={100} aria-label="传输进度">
            <div
              className={`h-full rounded-full transition-all ${
                transfer.status === "completed" ? "bg-success" : "bg-accent"
              }`}
              style={{ width: `${transfer.status === "completed" ? 100 : pct}%` }}
            />
          </div>
        </div>
      )}
      {transfer.error && (
        <div className="border-t border-border px-3 py-2 bg-background">
          <p
            className="text-xs text-destructive whitespace-pre-wrap break-all"
            style={{ fontFamily: "var(--font-mono)" }}
          >
            {transfer.error}
          </p>
        </div>
      )}
    </div>
  );
}
