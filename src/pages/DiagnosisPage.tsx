import { TopBar } from "@/components/layout/TopBar";
import { SessionSidebar } from "@/components/layout/SessionSidebar";
import { MainDiagnosisArea } from "@/components/layout/MainDiagnosisArea";

export function DiagnosisPage() {
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
