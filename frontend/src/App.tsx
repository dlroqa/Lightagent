import { Navigate, Route, Routes } from "react-router-dom";

import { Shell } from "./components/Shell";
import { Agent } from "./screens/Agent";
import { AgentTools } from "./screens/AgentTools";
import { DiagnosticsScreen } from "./screens/DiagnosticsScreen";
import { IdentityScreen } from "./screens/IdentityScreen";
import { RemoteCredentialsScreen } from "./screens/RemoteCredentialsScreen";
import { KnowledgeScreen } from "./screens/KnowledgeScreen";
import { MemoryScreen } from "./screens/MemoryScreen";
import { ProfilesScreen } from "./screens/ProfilesScreen";
import { SettingsScreen } from "./screens/SettingsScreen";

export function App() {
  return (
    <Routes>
      <Route element={<Shell />}>
        <Route index element={<Agent />} />
        <Route path="agent" element={<Navigate to="/" replace />} />
        <Route path="agent/tools" element={<Navigate to="/tools" replace />} />
        <Route path="tools" element={<AgentTools />} />
        <Route path="memory" element={<MemoryScreen />} />
        <Route path="knowledge" element={<KnowledgeScreen />} />
        <Route path="admin/identity" element={<IdentityScreen />} />
        <Route path="diagnostics" element={<DiagnosticsScreen />} />
        <Route path="admin/credentials" element={<RemoteCredentialsScreen />} />
        <Route path="profiles" element={<ProfilesScreen />} />
        <Route path="settings" element={<SettingsScreen />} />
        <Route path="*" element={<Navigate to="/" replace />} />
      </Route>
    </Routes>
  );
}
