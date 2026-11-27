import { HashRouter, Navigate, Route, Routes } from "react-router-dom";
import ErrorBoundary from "./components/ErrorBoundary";
import Sidebar from "./components/Sidebar";
import { useNodeEvents } from "./lib/nodeEvents";
import Dashboard from "./pages/Dashboard";
import Inbox from "./pages/Inbox";
import Machines from "./pages/Machines";
import Agents from "./pages/Agents";
import Tasks from "./pages/Tasks";
import Workflows from "./pages/Workflows";
import GitHub from "./pages/GitHub";
import Deploy from "./pages/Deploy";
import Sessions from "./pages/Sessions";
import Activity from "./pages/Activity";
import Usage from "./pages/Usage";
import Settings from "./pages/Settings";

export default function App() {
  useNodeEvents();
  return (
    <HashRouter>
      <div className="flex h-full">
        <Sidebar />
        <main className="min-w-0 flex-1 overflow-y-auto">
          <ErrorBoundary>
            <Routes>
              <Route path="/" element={<Dashboard />} />
              <Route path="/inbox" element={<Inbox />} />
              <Route path="/machines" element={<Machines />} />
              <Route path="/agents" element={<Agents />} />
              <Route path="/tasks" element={<Tasks />} />
              <Route path="/workflows" element={<Workflows />} />
              <Route path="/github" element={<GitHub />} />
              <Route path="/deploy" element={<Deploy />} />
              <Route path="/usage" element={<Usage />} />
              <Route path="/sessions" element={<Sessions />} />
              <Route path="/activity" element={<Activity />} />
              <Route path="/settings" element={<Settings />} />
              <Route path="*" element={<Navigate to="/" replace />} />
            </Routes>
          </ErrorBoundary>
        </main>
      </div>
    </HashRouter>
  );
}
