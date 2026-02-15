import { useEffect, useRef } from "react";
import { Terminal } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import { getWsClient } from "../ws";
import type { SessionInfo } from "../api";

import "@xterm/xterm/css/xterm.css";

interface TerminalPaneProps {
  session: SessionInfo;
  focused: boolean;
  onFocus: () => void;
}

export function TerminalPane({ session, focused, onFocus }: TerminalPaneProps) {
  const containerRef = useRef<HTMLDivElement>(null);
  const termRef = useRef<Terminal | null>(null);
  const fitRef = useRef<FitAddon | null>(null);

  useEffect(() => {
    const container = containerRef.current;
    if (!container) return;

    const term = new Terminal({
      fontSize: 14,
      fontFamily: "'JetBrains Mono', 'Fira Code', 'Cascadia Code', monospace",
      theme: {
        background: "#0d1117",
        foreground: "#e6edf3",
        cursor: "#58a6ff",
        selectionBackground: "#264f78",
        black: "#484f58",
        red: "#ff7b72",
        green: "#3fb950",
        yellow: "#d29922",
        blue: "#58a6ff",
        magenta: "#bc8cff",
        cyan: "#39d353",
        white: "#b1bac4",
        brightBlack: "#6e7681",
        brightRed: "#ffa198",
        brightGreen: "#56d364",
        brightYellow: "#e3b341",
        brightBlue: "#79c0ff",
        brightMagenta: "#d2a8ff",
        brightCyan: "#56d364",
        brightWhite: "#f0f6fc",
      },
      cursorBlink: true,
    });

    const fit = new FitAddon();
    term.loadAddon(fit);

    termRef.current = term;
    fitRef.current = fit;

    // Defer open+fit to next frame so container has layout dimensions
    const rafId = requestAnimationFrame(() => {
      term.open(container);
      fit.fit();

      // WS subscription
      const ws = getWsClient();
      ws.subscribe(
        session.id,
        (data) => term.write(data),
        (event) => {
          if (event.type === "session_exited") {
            term.write("\r\n\x1b[90m[session exited]\x1b[0m\r\n");
          }
        },
      );

      // Forward input
      inputDisposable = term.onData((data) => {
        ws.sendInput(session.id, data);
      });

      // Send initial resize
      const { cols, rows } = term;
      ws.sendResize(session.id, cols, rows);

      // ResizeObserver for fit
      observer = new ResizeObserver(() => {
        fit.fit();
        const { cols: c, rows: r } = term;
        ws.sendResize(session.id, c, r);
      });
      observer.observe(container);
    });

    let inputDisposable: { dispose: () => void } | null = null;
    let observer: ResizeObserver | null = null;

    return () => {
      cancelAnimationFrame(rafId);
      observer?.disconnect();
      inputDisposable?.dispose();
      getWsClient().unsubscribe(session.id);
      term.dispose();
      termRef.current = null;
      fitRef.current = null;
    };
  }, [session.id]);

  // Focus terminal element when pane is focused
  useEffect(() => {
    if (focused) {
      termRef.current?.focus();
    }
  }, [focused]);

  return (
    <div
      className={`terminal-pane ${focused ? "terminal-pane--focused" : ""}`}
      onClick={onFocus}
    >
      <div className="terminal-pane__header">
        <span className="terminal-pane__label">
          {session.label ?? session.id}
        </span>
        <span className="terminal-pane__id">{session.id.slice(0, 8)}</span>
      </div>
      <div className="terminal-pane__body" ref={containerRef} />
    </div>
  );
}
