export type SessionStatus = "active" | "closed";

export interface Session {
  id: string;
  env: string;
  service: string;
  symptom: string;
  status: SessionStatus;
}

export type RiskLevel = "read_only" | "low" | "high";

// 字段用 snake_case 与 Rust serde 序列化对齐（Tauri event payload 走 serde，不做 camelCase 转换）
export type AppEvent =
  | { type: "agent_started"; session_id: string; agent_pid: number }
  | { type: "tool_executing"; session_id: string; tool: string; args: unknown }
  | { type: "tool_result"; session_id: string; tool: string; output: unknown; elapsed_ms: number }
  | { type: "llm_thinking"; session_id: string; token: string }
  | { type: "confirm_required"; session_id: string; tool: string; args: unknown; risk_level: RiskLevel }
  | { type: "agent_stopped"; session_id: string }
  | { type: "agent_crashed"; session_id: string; reason: string }
  | { type: "diagnosis_done"; session_id: string; conclusion: string }
  | { type: "session_closed"; session_id: string }
  | { type: "session_deleted"; session_id: string };

export interface EventPayload {
  session_id: string;
  event: AppEvent;
}

export interface AgentRow {
  id: string;
  provider: string;
  display_name: string;
  path: string;
  version: string | null;
  source: "auto" | "manual";
  is_active: boolean;
  detected_at: string;
}

export interface SessionRow {
  id: string;
  title: string | null;
  status: "active" | "closed" | "archived";
  created_at: string;
  archived_at: string | null;
}

export type ChatPartType = "text" | "reasoning" | "tool";

export interface ToolCallInfo {
  name: string;
  args: unknown;
  status: "running" | "completed" | "error";
  output?: string;
  elapsedMs?: number;
}

export interface ChatPart {
  type: ChatPartType;
  text?: string;
  tool?: ToolCallInfo;
}

export type ChatMessageStatus = "streaming" | "done" | "stopped" | "error";

export interface ChatMessage {
  id: string;
  role: "user" | "agent";
  content: string;
  parts: ChatPart[];
  status: ChatMessageStatus;
}

export interface MessagePartRow {
  part_type: "text" | "tool";
  seq: number;
  text: string | null;
  tool_name: string | null;
  tool_args: string | null;
  tool_status: string | null;
  tool_output: string | null;
  tool_elapsed_ms: number | null;
}

export interface MessageRow {
  id: string;
  role: "user" | "agent";
  content: string | null;
  status: string | null;
  seq: number;
  parts: MessagePartRow[];
}
