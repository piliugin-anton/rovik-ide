import { onMount, onCleanup, createEffect } from 'solid-js';
import { Terminal as XTerm } from '@xterm/xterm';
import { FitAddon } from '@xterm/addon-fit';
import '@xterm/xterm/css/xterm.css';
import { spawnTerminal, writeToTerminal, resizeTerminal, closeTerminal } from '../lib/terminal';

interface TerminalProps {
  cwd: string | null;
  /** Shown and focused; inactive terminals stay alive, hidden. */
  active: boolean;
  onSpawned?: (shell: string) => void;
  /** The window title the shell set (OSC 0/2), e.g. "user@host: ~/project". */
  onTitleChange?: (title: string) => void;
  onExit?: (code: number | null) => void;
  /**
   * Panel shortcuts (new terminal, next/previous). Returning true keeps the
   * key from xterm, so it isn't also sent to the shell.
   */
  isPanelShortcut?: (e: KeyboardEvent) => boolean;
}

/** Ctrl with these keys, by physical key: the control characters of `[`, `\` and `]`. */
const CONTROL_PUNCTUATION: Record<string, string> = {
  BracketLeft: '\x1b',
  Backslash: '\x1c',
  BracketRight: '\x1d',
};

/**
 * The control character for Ctrl+key on a non-Latin keyboard layout
 * (Russian, Greek, Hebrew, Arabic…). The page is told the layout's letter
 * ("с" for the C key) and, in WebKit (Linux, macOS), no key code, so xterm
 * sent nothing: Ctrl+C couldn't stop a program, nor Ctrl+D, Ctrl+Z… The
 * physical key (`code`) says which one it is, as native terminals read it.
 * Null when xterm handles the key itself: Latin layouts, Ctrl+Shift
 * (shortcuts), Ctrl+Alt (AltGr on Windows), Cmd.
 */
export function controlCharFor(e: KeyboardEvent): string | null {
  if (e.type !== 'keydown' || !e.ctrlKey || e.altKey || e.metaKey || e.shiftKey) return null;
  if (e.key.length !== 1 || e.key.charCodeAt(0) < 128) return null;
  const letter = /^Key([A-Z])$/.exec(e.code);
  if (letter) return String.fromCharCode(letter[1].charCodeAt(0) - 64);
  return CONTROL_PUNCTUATION[e.code] ?? null;
}

export default function Terminal(props: TerminalProps) {
  let containerEl: HTMLDivElement | undefined;
  let xterm: XTerm | undefined;
  let fitAddon: FitAddon | undefined;
  let terminalId: string | null = null;
  let disposed = false;
  let exited = false;

  /** Fit to the container; skipped while hidden (xterm can't lay out at 0×0). */
  function fit() {
    if (!containerEl || containerEl.clientWidth === 0 || containerEl.clientHeight === 0) return;
    fitAddon?.fit();
  }

  onMount(() => {
    if (!containerEl) return;

    const term = new XTerm({
      convertEol: true,
      cursorBlink: true,
      fontSize: 13,
      lineHeight: 1.3,
      fontFamily: "Menlo, Consolas, 'Courier New', monospace",
      scrollback: 5000,
      theme: {
        background: '#1e1e1e',
        foreground: '#d4d4d4',
        cursor: '#d4d4d4',
        selectionBackground: '#094771',
      },
    });
    xterm = term;
    fitAddon = new FitAddon();
    term.loadAddon(fitAddon);
    term.open(containerEl);
    fit();

    term.attachCustomKeyEventHandler((e) => {
      if (props.isPanelShortcut?.(e)) return false;
      // Ctrl+C and the like on a non-Latin layout: sent as typed.
      const control = controlCharFor(e);
      if (control) {
        e.preventDefault();
        term.input(control, true);
        return false;
      }
      return true;
    });
    term.onTitleChange((title) => props.onTitleChange?.(title));

    // Input typed before the shell is up is kept, not dropped.
    let queued = '';
    term.onData((data) => {
      if (exited) return;
      if (!terminalId) {
        queued += data;
        return;
      }
      void writeToTerminal(terminalId, data).catch(() => {});
    });
    term.onResize(({ cols, rows }) => {
      if (terminalId && !exited) void resizeTerminal(terminalId, cols, rows).catch(() => {});
    });

    spawnTerminal(props.cwd, term.cols, term.rows, (event) => {
      if (disposed) return;
      if (event.type === 'output') {
        term.write(event.data);
      } else {
        exited = true;
        const code = event.code == null ? '' : ` with code ${event.code}`;
        term.write(`\r\n\x1b[2m[process exited${code}]\x1b[0m\r\n`);
        props.onExit?.(event.code);
      }
    })
      .then((info) => {
        if (disposed) {
          void closeTerminal(info.id);
          return;
        }
        terminalId = info.id;
        props.onSpawned?.(info.shell);
        // The size may have changed while spawning (panel opened, refit).
        void resizeTerminal(info.id, term.cols, term.rows).catch(() => {});
        if (queued) {
          void writeToTerminal(info.id, queued).catch(() => {});
          queued = '';
        }
      })
      .catch((err) => {
        if (disposed) return;
        exited = true;
        term.write(`\x1b[31mCouldn't start a shell: ${String(err)}\x1b[0m\r\n`);
      });

    // Refit whenever the container changes size: panel expand, sidebar
    // resize, window resize, or this terminal becoming the visible one.
    const resizeObserver = new ResizeObserver(fit);
    resizeObserver.observe(containerEl);
    onCleanup(() => resizeObserver.disconnect());
  });

  // Switched to: refit (its size may have changed while hidden) and focus.
  createEffect(() => {
    if (!props.active) return;
    requestAnimationFrame(() => {
      if (disposed || !props.active) return;
      fit();
      xterm?.focus();
    });
  });

  onCleanup(() => {
    disposed = true;
    if (terminalId && !exited) void closeTerminal(terminalId);
    xterm?.dispose();
  });

  return (
    <div
      class="terminal-instance"
      classList={{ hidden: !props.active }}
      ref={containerEl}
      role="tabpanel"
    />
  );
}
