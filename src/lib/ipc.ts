import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import type { EventPayload } from "@/lib/types";

export async function startDiagnosis(env: string, service: string, symptom: string): Promise<string> {
  return invoke<string>("start_diagnosis_cmd", { env, service, symptom });
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

export async function cancelDiagnosis(sessionId: string): Promise<void> {
  return invoke<void>("cancel_diagnosis_cmd", { sessionId });
}

export async function onAppEvent(handler: (payload: EventPayload) => void): Promise<() => void> {
  const unlisten = await listen<EventPayload>("app_event", (event) => {
    handler(event.payload);
  });
  return unlisten;
}
