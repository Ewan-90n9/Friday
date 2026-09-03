import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import type { EventPayload, AgentRow, SessionRow, MessageRow, ToolInfo, EnvironmentRow, EnvCredentialRow, TestConnectionResult, CredentialInput, SaveEnvironmentResult } from "@/lib/types";

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

export async function deleteEnvironment(id: string): Promise<void> {
  return invoke<void>("delete_environment_cmd", { id });
}

export async function testConnection(params: {
  environmentId?: string | null;
  credentialId?: string | null;
  host: string;
  port?: number;
  user: string;
  authType: string;
  privateKeyPath?: string | null;
  password?: string | null;
}): Promise<TestConnectionResult> {
  return invoke<TestConnectionResult>("test_connection_params_cmd", {
    environmentId: params.environmentId ?? null,
    credentialId: params.credentialId ?? null,
    host: params.host,
    port: params.port ?? null,
    user: params.user,
    authType: params.authType,
    privateKeyPath: params.privateKeyPath ?? null,
    password: params.password ?? null,
  });
}

export async function getArtifactoryBaseUrl(): Promise<string> {
  return invoke<string>("get_artifactory_base_url_cmd");
}

export async function setArtifactoryBaseUrl(url: string): Promise<void> {
  return invoke<void>("set_artifactory_base_url_cmd", { url });
}

export async function getAutoApproveTools(): Promise<boolean> {
  return invoke<boolean>("get_auto_approve_tools_cmd");
}

export async function setAutoApproveTools(enabled: boolean): Promise<void> {
  return invoke<void>("set_auto_approve_tools_cmd", { enabled });
}

export async function listEnvCredentials(environmentId: string): Promise<EnvCredentialRow[]> {
  return invoke<EnvCredentialRow[]>("list_env_credentials_cmd", { environmentId });
}

export async function saveEnvironment(params: {
  environmentId?: string | null;
  name: string;
  host: string;
  port?: number;
  credentials: CredentialInput[];
}): Promise<SaveEnvironmentResult> {
  return invoke<SaveEnvironmentResult>("save_environment_cmd", {
    params: {
      environmentId: params.environmentId ?? null,
      name: params.name,
      host: params.host,
      port: params.port ?? null,
      credentials: params.credentials,
    },
  });
}
