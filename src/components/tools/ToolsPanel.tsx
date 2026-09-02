import { useEffect, useMemo, useState } from "react";
import {
  Wrench,
  CircleNotch,
  CaretRight,
  CaretDown,
  Desktop,
  Cpu,
  ChartPie,
  Terminal,
  ArrowsLeftRight,
  Gear,
} from "@phosphor-icons/react";
import type { Icon } from "@phosphor-icons/react";
import { listTools } from "@/lib/ipc";
import type { ToolCategory, ToolInfo } from "@/lib/types";

const RISK_LABELS: Record<string, { label: string; className: string }> = {
  read_only: { label: "只读", className: "bg-success/10 text-success border-success/20" },
  low: { label: "低", className: "bg-warning/10 text-warning border-warning/20" },
  high: { label: "高", className: "bg-destructive/10 text-destructive border-destructive/20" },
};

// 分组展示顺序沿诊断流程：定位环境/进程 → JVM 基础诊断 → 堆分析 → Arthas → 文件传输 → 通用
// 与后端 tools/category.rs 的 ToolCategory 声明序一致
const CATEGORY_META: { key: ToolCategory; label: string; icon: Icon }[] = [
  { key: "environment", label: "环境与进程", icon: Desktop },
  { key: "jvm", label: "JVM 诊断", icon: Cpu },
  { key: "heap", label: "堆快照分析", icon: ChartPie },
  { key: "arthas", label: "Arthas 动态诊断", icon: Terminal },
  { key: "file_transfer", label: "文件传输", icon: ArrowsLeftRight },
  { key: "builtin", label: "通用", icon: Gear },
];

export function ToolsPanel() {
  const [tools, setTools] = useState<ToolInfo[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  // 全部默认折叠；仅组件内 state，不持久化
  const [collapsed, setCollapsed] = useState<Record<ToolCategory, boolean>>({
    environment: true,
    jvm: true,
    heap: true,
    arthas: true,
    file_transfer: true,
    builtin: true,
  });

  useEffect(() => {
    listTools()
      .then(setTools)
      .catch((e) => setError(String(e)));
  }, []);

  // 后端已按 category → name 排序，按到达序分桶即可；未知 category 回退通用组
  const grouped = useMemo(() => {
    const buckets = new Map<ToolCategory, ToolInfo[]>();
    for (const meta of CATEGORY_META) buckets.set(meta.key, []);
    for (const tool of tools ?? []) {
      const key = buckets.has(tool.category) ? tool.category : "builtin";
      buckets.get(key)!.push(tool);
    }
    return buckets;
  }, [tools]);

  return (
    <section className="flex-1 flex flex-col min-h-0">
      {/* Header */}
      <div className="flex items-center gap-2 h-10 px-4 border-b border-border shrink-0">
        <Wrench size={14} weight="regular" className="text-muted-foreground" aria-hidden="true" />
        <span
          className="text-xs font-medium text-muted-foreground uppercase tracking-wide"
          style={{ fontFamily: "var(--font-mono)" }}
        >
          诊断工具
        </span>
        {tools && (
          <span
            className="text-xs text-muted-foreground/60 ml-auto"
            style={{ fontFamily: "var(--font-mono)" }}
          >
            {tools.length}
          </span>
        )}
      </div>

      {/* Grouped tool list */}
      <div className="flex-1 overflow-y-auto px-3 py-3">
        {error && (
          <div className="text-destructive text-xs px-1 py-2">{error}</div>
        )}
        {tools === null && !error && (
          <div className="flex items-center justify-center gap-2 py-8 text-muted-foreground text-xs">
            <CircleNotch size={14} weight="regular" className="animate-spin" aria-hidden="true" />
            加载中…
          </div>
        )}
        {tools !== null && tools.length === 0 && (
          <div className="py-8 text-center text-muted-foreground text-xs leading-relaxed">
            暂无已注册工具
          </div>
        )}
        {tools !== null &&
          tools.length > 0 &&
          CATEGORY_META.map((meta) => {
            const items = grouped.get(meta.key)!;
            // 后端数据缺失的分类不渲染空组头
            if (items.length === 0) return null;
            const isCollapsed = collapsed[meta.key];
            const GroupIcon = meta.icon;
            return (
              <div key={meta.key} className="mb-1">
                <button
                  type="button"
                  aria-expanded={!isCollapsed}
                  onClick={() =>
                    setCollapsed((c) => ({ ...c, [meta.key]: !c[meta.key] }))
                  }
                  className="w-full flex items-center gap-1.5 px-1.5 py-1.5 rounded-md hover:bg-surface-2/60 transition-colors text-left"
                >
                  {isCollapsed ? (
                    <CaretRight
                      size={12}
                      weight="bold"
                      className="text-muted-foreground shrink-0"
                      aria-hidden="true"
                    />
                  ) : (
                    <CaretDown
                      size={12}
                      weight="bold"
                      className="text-muted-foreground shrink-0"
                      aria-hidden="true"
                    />
                  )}
                  <GroupIcon
                    size={12}
                    className="text-muted-foreground shrink-0"
                    aria-hidden="true"
                  />
                  <span className="text-xs font-medium text-foreground/90">{meta.label}</span>
                  <span
                    className="ml-auto text-xs text-muted-foreground/60"
                    style={{ fontFamily: "var(--font-mono)" }}
                  >
                    {items.length}
                  </span>
                </button>
                {!isCollapsed && (
                  <ul className="flex flex-col mt-0.5">
                    {items.map((tool) => {
                      const risk = RISK_LABELS[tool.risk_level] ?? {
                        label: tool.risk_level,
                        className: "bg-muted/50 text-muted-foreground border-border",
                      };
                      // opencode 客户端按 MCP server name 给工具加 friday_ 前缀
                      // （注册表存无前缀名），展示层补齐前缀与聊天流工具卡片一致
                      const displayName = `friday_${tool.name}`;
                      return (
                        <li key={tool.name} className="px-2.5 py-1.5 pl-5">
                          <div className="flex items-center gap-1.5">
                            <code
                              className="text-xs text-foreground font-medium truncate"
                              style={{ fontFamily: "var(--font-mono)" }}
                              title={displayName}
                            >
                              {displayName}
                            </code>
                            <span
                              className={`shrink-0 ml-auto px-1.5 py-px rounded text-[10px] border ${risk.className}`}
                              style={{ fontFamily: "var(--font-mono)" }}
                            >
                              {risk.label}
                            </span>
                          </div>
                          <p
                            className="text-xs text-muted-foreground truncate"
                            title={tool.description}
                          >
                            {tool.description}
                          </p>
                        </li>
                      );
                    })}
                  </ul>
                )}
              </div>
            );
          })}
      </div>
    </section>
  );
}
