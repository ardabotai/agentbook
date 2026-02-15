import { useCallback, useEffect, useState } from "react";
import { Toolbar } from "./components/Toolbar";
import { TerminalGrid } from "./components/TerminalGrid";
import { createSession, listSessions, type SessionInfo } from "./api";

export function App() {
  const [sessions, setSessions] = useState<SessionInfo[]>([]);
  const [creating, setCreating] = useState(false);

  // Load existing sessions on mount
  useEffect(() => {
    listSessions()
      .then(setSessions)
      .catch((err) => console.error("Failed to load sessions:", err));
  }, []);

  const handleNewSession = useCallback(async () => {
    setCreating(true);
    try {
      const session = await createSession({
        label: `shell-${sessions.length + 1}`,
      });
      setSessions((prev) => [...prev, session]);
    } catch (err) {
      console.error("Failed to create session:", err);
    } finally {
      setCreating(false);
    }
  }, [sessions.length]);

  return (
    <div className="app">
      <Toolbar
        sessionCount={sessions.length}
        onNewSession={handleNewSession}
        creating={creating}
      />
      <TerminalGrid sessions={sessions} />
    </div>
  );
}
