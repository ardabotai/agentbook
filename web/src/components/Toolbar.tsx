interface ToolbarProps {
  sessionCount: number;
  onNewSession: () => void;
  creating: boolean;
}

export function Toolbar({
  sessionCount,
  onNewSession,
  creating,
}: ToolbarProps) {
  return (
    <header className="toolbar">
      <div className="toolbar__brand">
        <span className="toolbar__logo">tmax</span>
        <span className="toolbar__badge">{sessionCount} sessions</span>
      </div>
      <button
        className="toolbar__button"
        onClick={onNewSession}
        disabled={creating}
      >
        {creating ? "Creating..." : "+ New Session"}
      </button>
    </header>
  );
}
