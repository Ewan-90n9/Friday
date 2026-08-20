import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import type { EventPayload, AgentRow, SessionRow } from "@/lib/types";

export async function sendMessage(sessionId: string | null, message: string): Promise<string> {
  return invoke<string>("send_message_cmd", { sessionId: sessionId, message: message });
}

export async function listSessions(): Promise<SessionRow[]> {
  return invoke<SessionRow[]>("list_sessions_cmd");
}

export async function stopAgent(sessionId: string): Promise<void> {
  return invoke<void>("stop_agent_cmd", { sessionId });
}

export async function closeSession(sessionId: string): Promise<void> {
  return invoke<void>("close_session_cmd", { sessionId });
}

export async function confirmTool(sessionId: string, tool: string): Promise<void> {
  return invoke<void>("confirm_tool_cmd", { sessionId, tool });
}

export async function onAppEvent(handler: (payload: EventPayload) => void): Promise<() => void> {
  const unlisten = await listen<EventPayload>("app_event", (event) => {
    handler(event.payload);
  });
  return unlisten;
}

export async function detectAgents(): Promise<void> {
  return invoke<void>("detect_agents_cmd");
}

export async function listAgents(): Promise<AgentRow[]> {
  return invoke<AgentRow[]>("list_agents_cmd");
}

export async function addAgent(provider: string, path: string): Promise<AgentRow> {
  return invoke<AgentRow>("add_agent_cmd", { provider, path });
}

export async function setActiveAgent(id: string): Promise<void> {
  return invoke<void>("set_active_agent_cmd", { id });
}

export async function removeAgent(id: string): Promise<void> {
  return invoke<void>("remove_agent_cmd", { id });
}

export async function setLogLevel(level: string): Promise<void> {
  return invoke<void>("set_log_level_cmd", { level });
}
