import { useEffect } from "react";
import { TopBar } from "@/components/layout/TopBar";
import { SessionSidebar } from "@/components/layout/SessionSidebar";
import { MainDiagnosisArea } from "@/components/layout/MainDiagnosisArea";
import { useAgentStore } from "@/store/agentStore";
import { useSessionStore } from "@/store/sessionStore";

export function DiagnosisPage() {
  const refreshAgents = useAgentStore((s) => s.refresh);
  const loadSessions = useSessionStore((s) => s.loadSessions);
  const initEventListener = useSessionStore((s) => s.initEventListener);

  useEffect(() => {
    refreshAgents();
    loadSessions();
    initEventListener();
  }, [refreshAgents, loadSessions, initEventListener]);

  return (
    <div className="flex flex-col h-screen bg-background">
      <TopBar />
      <div className="flex flex-1 min-h-0">
        <SessionSidebar />
        <MainDiagnosisArea />
      </div>
    </div>
  );
}
