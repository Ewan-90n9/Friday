import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import type { EventPayload, AgentRow, SessionRow, MessageRow, ToolInfo, EnvironmentRow, TestConnectionResult } from "@/lib/types";

export async function sendMessage(sessionId: string | null, message: string): Promise<string> {
  return invoke<string>("send_message_cmd", { sessionId: sessionId, message: message });
}

export async function listSessions(includeArchived: boolean = false): Promise<SessionRow[]> {
  return invoke<SessionRow[]>("list_sessions_cmd", { includeArchived });
}

export async function stopAgent(sessionId: string): Promise<void> {
  return invoke<void>("stop_agent_cmd", { sessionId });
}

export async function closeSession(sessionId: string): Promise<void> {
  return invoke<void>("close_session_cmd", { sessionId });
}

export async function getSessionMessages(sessionId: string): Promise<MessageRow[]> {
  return invoke<MessageRow[]>("get_session_messages_cmd", { sessionId });
}

export async function archiveSession(sessionId: string): Promise<void> {
  return invoke<void>("archive_session_cmd", { sessionId });
}

export async function unarchiveSession(sessionId: string): Promise<void> {
  return invoke<void>("unarchive_session_cmd", { sessionId });
}

export async function deleteSession(sessionId: string): Promise<void> {
  return invoke<void>("delete_session_cmd", { sessionId });
}

export async function confirmTool(confirmId: string, approved: boolean): Promise<void> {
  return invoke<void>("confirm_tool_cmd", { confirmId, approved });
}

export async function listTools(): Promise<ToolInfo[]> {
  return invoke<ToolInfo[]>("list_tools_cmd");
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

export async function listEnvironments(): Promise<EnvironmentRow[]> {
  return invoke<EnvironmentRow[]>("list_environments_cmd");
}

export async function addEnvironment(params: {
  name: string;
  host: string;
  port?: number;
  user: string;
  authType: string;
  privateKeyPath?: string | null;
  password?: string | null;
}): Promise<EnvironmentRow> {
  return invoke<EnvironmentRow>("add_environment_cmd", {
    name: params.name,
    host: params.host,
    port: params.port ?? null,
    user: params.user,
    authType: params.authType,
    privateKeyPath: params.privateKeyPath ?? null,
    password: params.password ?? null,
  });
}

export async function updateEnvironment(params: {
  id: string;
  name: string;
  host: string;
  port?: number;
  user: string;
  authType: string;
  privateKeyPath?: string | null;
  password?: string | null;
}): Promise<void> {
  return invoke<void>("update_environment_cmd", {
    id: params.id,
    name: params.name,
    host: params.host,
    port: params.port ?? null,
    user: params.user,
    authType: params.authType,
    privateKeyPath: params.privateKeyPath ?? null,
    password: params.password ?? null,
  });
}

export async function deleteEnvironment(id: string): Promise<void> {
  return invoke<void>("delete_environment_cmd", { id });
}

export async function testConnection(id: string): Promise<TestConnectionResult> {
  return invoke<TestConnectionResult>("test_connection_cmd", { id });
}
