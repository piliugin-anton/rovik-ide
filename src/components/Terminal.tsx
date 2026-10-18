import { onMount, onCleanup, createEffect, createSignal, Show } from 'solid-js';
import { Terminal as XTerm } from '@xterm/xterm';
import { FitAddon } from '@xterm/addon-fit';
import '@xterm/xterm/css/xterm.css';
import {
  spawnRemoteTerminal,
  spawnTerminal,
  writeToTerminal,
  resizeTerminal,
  closeTerminal,
  type TerminalEvent,
  type TerminalHost,
} from '../lib/terminal';
import TerminalContextMenu from './TerminalContextMenu';

interface TerminalProps {
  cwd: string | null;
  /** The cloud host its shell runs on, for its whole life; none on this computer. */
  host?: TerminalHost | null;
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

/** How long the terminal's size has to hold still before it's fitted to. */
const SETTLE_MS = 150;

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

/** `text` without control characters, which the terminal would act on. */
const printable = (text: string) => text.replace(/[\u0000-\u001f\u007f-\u009f]/g, '');

/** Copy text to the OS clipboard, with a fallback for webviews without the async API. */
async function writeClipboard(text: string): Promise<boolean> {
  try {
    await navigator.clipboard.writeText(text);
    return true;
  } catch {
    const area = document.createElement('textarea');
    area.value = text;
    area.style.position = 'fixed';
    area.style.opacity = '0';
    document.body.append(area);
    area.select();
    let ok = false;
    try {
      ok = document.execCommand('copy');
    } catch {
      ok = false;
    }
    area.remove();
    return ok;
  }
}

/** Ctrl+Shift+C / Ctrl+Shift+V by physical key, so non-Latin layouts work too. */
function isCopyKey(e: KeyboardEvent): boolean {
  return (
    e.type === 'keydown' &&
    e.ctrlKey &&
    e.shiftKey &&
    !e.altKey &&
    !e.metaKey &&
    (e.key.toLowerCase() === 'c' || e.code === 'KeyC')
  );
}

function isPasteKey(e: KeyboardEvent): boolean {
  return (
    e.type === 'keydown' &&
    e.ctrlKey &&
    e.shiftKey &&
    !e.altKey &&
    !e.metaKey &&
    (e.key.toLowerCase() === 'v' || e.code === 'KeyV')
  );
}

export default function Terminal(props: TerminalProps) {
  let containerEl: HTMLDivElement | undefined;
  let xterm: XTerm | undefined;
  let fitAddon: FitAddon | undefined;
  let terminalId: string | null = null;
  let disposed = false;
  let exited = false;

  const [menuVisible, setMenuVisible] = createSignal(false);
  const [menuPos, setMenuPos] = createSignal({ x: 0, y: 0 });
  const [menuHasSelection, setMenuHasSelection] = createSignal(false);
  let menuEl: HTMLDivElement | undefined;

  /** Clamp to the viewport once the menu's size is known (like App's file menu). */
  function setMenuEl(el: HTMLDivElement) {
    menuEl = el;
    const pos = menuPos();
    const w = el.offsetWidth || 160;
    const h = el.offsetHeight || 120;
    const x = Math.max(0, Math.min(pos.x, window.innerWidth - w - 10));
    const y = Math.max(0, Math.min(pos.y, window.innerHeight - h - 10));
    if (x !== pos.x || y !== pos.y) setMenuPos({ x, y });
  }

  const closeMenu = () => setMenuVisible(false);

  async function copySelection() {
    const term = xterm;
    if (!term || !term.hasSelection()) return;
    const text = term.getSelection();
    if (!text) return;
    await writeClipboard(text);
    term.focus();
  }

  async function pasteFromClipboard() {
    const term = xterm;
    if (!term) return;
    try {
      const text = await navigator.clipboard.readText();
      if (text) term.paste(text);
    } catch {
      // Denied permission or no async clipboard API: nothing to paste.
    }
    term.focus();
  }

  function selectAll() {
    const term = xterm;
    if (!term) return;
    term.selectAll();
    term.focus();
  }

  function handleContextMenu(e: MouseEvent) {
    // Keep App's file-tree menu from opening over the terminal.
    e.preventDefault();
    e.stopPropagation();
    setMenuHasSelection(xterm?.hasSelection() ?? false);
    setMenuPos({ x: e.clientX, y: e.clientY });
    setMenuVisible(true);
  }

  /** Fit to the container; skipped while hidden (xterm can't lay out at 0×0). */
  function fit() {
    if (!containerEl || containerEl.clientWidth === 0 || containerEl.clientHeight === 0) return;
    fitAddon?.fit();
  }

  // A size still changing is fitted to once it holds still. The chat panel
  // folding back from the whole window widens the terminal from nothing
  // over a third of a second: fitted on every frame, the shell was resized
  // to 2 columns, then 14, 59… back to 100, and redrew its prompt at each,
  // which pushed the output away and left the prompt in pieces.
  let settling: ReturnType<typeof setTimeout> | undefined;
  function fitWhenSettled() {
    clearTimeout(settling);
    settling = setTimeout(fit, SETTLE_MS);
  }
  onCleanup(() => clearTimeout(settling));

  // Dismiss the menu on any outside press, scroll, resize, blur, or Escape —
  // the same triggers App uses for the file-tree menu.
  createEffect(() => {
    if (!menuVisible()) return;
    const outside = (e: Event) => {
      if (menuEl && !menuEl.contains(e.target as Node)) closeMenu();
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') {
        e.preventDefault();
        closeMenu();
      }
    };
    window.addEventListener('pointerdown', outside, true);
    window.addEventListener('contextmenu', outside, true);
    window.addEventListener('scroll', outside, true);
    window.addEventListener('keydown', onKey, true);
    window.addEventListener('blur', closeMenu);
    window.addEventListener('resize', closeMenu);
    onCleanup(() => {
      window.removeEventListener('pointerdown', outside, true);
      window.removeEventListener('contextmenu', outside, true);
      window.removeEventListener('scroll', outside, true);
      window.removeEventListener('keydown', onKey, true);
      window.removeEventListener('blur', closeMenu);
      window.removeEventListener('resize', closeMenu);
    });
  });

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
      // Ctrl+Shift+C copies the selection, Ctrl+Shift+V pastes: handled
      // here so nothing is sent to the shell. Plain Ctrl+C still interrupts.
      if (isCopyKey(e)) {
        if (term.hasSelection()) void copySelection();
        return false;
      }
      if (isPasteKey(e)) {
        // Linux's and Windows' webviews paste on Ctrl+Shift+V too, once the
        // key is handled (WebKitGTK's "paste as plain text", Chromium's
        // "paste and match style"), and xterm would take that paste as
        // well: the text went in twice.
        e.preventDefault();
        void pasteFromClipboard();
        return false;
      }
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

    // A host takes a moment to reach: said until its shell's first output.
    const host = props.host;
    let connecting = false;
    if (host) {
      connecting = true;
      term.write(`\x1b[2mConnecting to ${printable(host.name)}…\x1b[0m`);
    }
    const connected = () => {
      if (!connecting) return;
      connecting = false;
      term.write('\r\x1b[2K');
    };

    const onEvent = (event: TerminalEvent) => {
      if (disposed) return;
      connected();
      if (event.type === 'output') {
        term.write(event.data);
      } else {
        exited = true;
        const code = event.code == null ? '' : ` with code ${event.code}`;
        // A shell on a host that ends with no exit status lost its link.
        const ended = host && event.code == null ? 'connection closed' : `process exited${code}`;
        term.write(`\r\n\x1b[2m[${ended}]\x1b[0m\r\n`);
        props.onExit?.(event.code);
      }
    };
    const spawned = host
      ? spawnRemoteTerminal(host.id, term.cols, term.rows, onEvent)
      : spawnTerminal(props.cwd, term.cols, term.rows, onEvent);
    spawned
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
        connected();
        const what = host ? `open a shell on ${printable(host.name)}` : 'start a shell';
        term.write(`\x1b[31mCouldn't ${what}: ${String(err)}\x1b[0m\r\n`);
      });

    // Refit whenever the container changes size: panel expand, sidebar
    // resize, window resize, or this terminal becoming the visible one.
    const resizeObserver = new ResizeObserver(fitWhenSettled);
    resizeObserver.observe(containerEl);
    onCleanup(() => resizeObserver.disconnect());
  });

  // Switched to: refit (its size may have changed while hidden) and focus.
  // Switched away (e.g. keyboard tab switch): close the menu, which is
  // position:fixed and would otherwise linger over the other tab while
  // still acting on this hidden terminal.
  createEffect(() => {
    if (!props.active) closeMenu();
  });
  createEffect(() => {
    if (!props.active) return;
    requestAnimationFrame(() => {
      if (disposed || !props.active) return;
      fitWhenSettled();
      xterm?.focus();
    });
  });

  onCleanup(() => {
    disposed = true;
    if (terminalId && !exited) void closeTerminal(terminalId);
    xterm?.dispose();
  });

  return (
    <>
      <div
        class="terminal-instance"
        classList={{ hidden: !props.active }}
        ref={containerEl}
        role="tabpanel"
        onContextMenu={handleContextMenu}
      />
      <Show when={menuVisible()}>
        <TerminalContextMenu
          ref={setMenuEl}
          x={menuPos().x}
          y={menuPos().y}
          hasSelection={menuHasSelection()}
          onCopy={() => {
            closeMenu();
            void copySelection();
          }}
          onPaste={() => {
            closeMenu();
            void pasteFromClipboard();
          }}
          onSelectAll={() => {
            closeMenu();
            selectAll();
          }}
        />
      </Show>
    </>
  );
}
