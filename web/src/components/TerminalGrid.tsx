import { useState } from "react";
import { TerminalPane } from "./TerminalPane";
import type { SessionInfo } from "../api";

interface TerminalGridProps {
  sessions: SessionInfo[];
}

export function TerminalGrid({ sessions }: TerminalGridProps) {
  const [focusedId, setFocusedId] = useState<string | null>(null);

  if (sessions.length === 0) {
    return (
      <div className="terminal-grid__empty">
        <p>No sessions yet. Click "New Session" to get started.</p>
      </div>
    );
  }

  return (
    <div className="terminal-grid">
      {sessions.map((s) => (
        <TerminalPane
          key={s.id}
          session={s}
          focused={focusedId === s.id}
          onFocus={() => setFocusedId(s.id)}
        />
      ))}
    </div>
  );
}
