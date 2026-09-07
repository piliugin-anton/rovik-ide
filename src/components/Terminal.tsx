import { onMount, onCleanup, createSignal } from 'solid-js';
import { Terminal as XTerm } from '@xterm/xterm';
import { FitAddon } from '@xterm/addon-fit';
import '@xterm/xterm/css/xterm.css';
import type { UnlistenFn } from '@tauri-apps/api/event';
import {
  spawnTerminal,
  writeToTerminal,
  resizeTerminal,
  closeTerminal,
  onTerminalData,
  onTerminalExit,
} from '../lib/terminal';

interface TerminalProps {
  cwd: string | null;
  onExit?: () => void;
}

export default function Terminal(props: TerminalProps) {
  let containerEl: HTMLDivElement | undefined;
  let xterm: XTerm | undefined;
  let fitAddon: FitAddon | undefined;
  let resizeObserver: ResizeObserver | undefined;
  let unlistenData: UnlistenFn | undefined;
  let unlistenExit: UnlistenFn | undefined;

  const [terminalId, setTerminalId] = createSignal<string | null>(null);

  onMount(async () => {
    if (!containerEl) return;

    xterm = new XTerm({
      convertEol: true,
      cursorBlink: true,
      fontSize: 13,
      lineHeight: 1.3,
      fontFamily: "Menlo, Consolas, 'Courier New', monospace",
      theme: {
        background: '#1e1e1e',
        foreground: '#d4d4d4',
        cursor: '#d4d4d4',
        selectionBackground: '#094771',
      },
    });

    fitAddon = new FitAddon();
    xterm.loadAddon(fitAddon);
    xterm.open(containerEl);
    fitAddon.fit();

    const id = await spawnTerminal(props.cwd, xterm.cols, xterm.rows);
    setTerminalId(id);

    unlistenData = await onTerminalData(id, (data) => xterm?.write(data));
    unlistenExit = await onTerminalExit(id, () => {
      xterm?.write('\r\n\x1b[2m[process exited]\x1b[0m\r\n');
      props.onExit?.();
    });

    xterm.onData((data) => {
      void writeToTerminal(id, data);
    });

    xterm.onResize(({ cols, rows }) => {
      void resizeTerminal(id, cols, rows);
    });

    // Refit whenever the container changes size (panel expand/collapse,
    // sidebar resize, window resize). Skip while hidden (0 size) to avoid
    // xterm choking on a zero-size layout.
    resizeObserver = new ResizeObserver(() => {
      if (!containerEl) return;
      if (containerEl.clientWidth === 0 || containerEl.clientHeight === 0) return;
      fitAddon?.fit();
    });
    resizeObserver.observe(containerEl);
  });

  onCleanup(() => {
    resizeObserver?.disconnect();
    unlistenData?.();
    unlistenExit?.();
    const id = terminalId();
    if (id) void closeTerminal(id);
    xterm?.dispose();
  });

  return <div class="terminal-instance" ref={containerEl} />;
}
