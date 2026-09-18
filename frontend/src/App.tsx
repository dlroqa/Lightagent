import { Navigate, Route, Routes } from "react-router-dom";

import { Shell } from "./components/Shell";
import { Agent } from "./screens/Agent";
import { AgentTools } from "./screens/AgentTools";
import { SettingsScreen } from "./screens/SettingsScreen";

export function App() {
  return (
    <Routes>
      <Route element={<Shell />}>
        <Route index element={<Agent />} />
        <Route path="agent" element={<Navigate to="/" replace />} />
        <Route path="agent/tools" element={<Navigate to="/tools" replace />} />
        <Route path="tools" element={<AgentTools />} />
        <Route path="settings" element={<SettingsScreen />} />
        <Route path="*" element={<Navigate to="/" replace />} />
      </Route>
    </Routes>
  );
}
