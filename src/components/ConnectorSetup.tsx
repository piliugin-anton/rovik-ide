// ConnectorSetup.tsx
//
// A connector's "Set up" and "Sign out" (Settings → Connectors), on the
// computer its chats run on: the flow of lib/connectorSetup.ts, with what
// each step prints, the sign-in page it gives (opened in this computer's
// browser when the step runs on a cloud host; a program here opens it
// itself), and a field for the code that page shows, or, where the page
// asks for a code the step printed (a device code), where to read it. The
// same for every connector: what the steps run is the connector's own, in
// the core.
import { createSignal, onCleanup, onMount, Show } from 'solid-js';
import { invoke } from '@tauri-apps/api/core';
import type { Provider } from '../types';
import { errorText, type ConnectorProbe } from '../lib/connectors';
import {
  runSetupStep,
  setUpConnector,
  setupCall,
  SetupDeclined,
  stepTitle,
  type SetupAction,
  type SetupJob,
} from '../lib/connectorSetup';
import { confirmDialog } from './ConfirmDialog';

interface ConnectorSetupProps {
  /** The connector, with the settings being edited. */
  connector: Provider;
  /** Its program, for the user: `Claude Code`. */
  title: string;
  /** Where its chats run, and so where it's set up: a host's id, or null for this computer. */
  host: string | null;
  /** For the user: `on this computer`, `on Build server`. */
  where: string;
  /**
   * "Test & refresh models", as the page does it: shows and keeps what it
   * found. What failed it throws without showing it: Set up acts on it (a
   * missing program, it installs) or shows it as its own result.
   */
  probe: () => Promise<ConnectorProbe>;
  /** Signed in, as the last test found: Sign out is offered. */
  signedIn: boolean;
  /** A newer version, as the last test found: Update is offered. */
  update?: string | null;
  /** Starts at once (the list's "Set up"). */
  autoStart?: boolean;
}

export default function ConnectorSetup(props: ConnectorSetupProps) {
  const [stage, setStage] = createSignal<string | null>(null);
  const [job, setJob] = createSignal<SetupJob | null>(null);
  const [result, setResult] = createSignal<{ ok: boolean; text: string } | null>(null);
  const [code, setCode] = createSignal('');
  const [codeError, setCodeError] = createSignal<string | null>(null);
  let abort: AbortController | null = null;
  // The step whose sign-in page was opened for the user, once.
  let opened = '';

  const call = () => setupCall(props.host);

  function onJob(next: SetupJob) {
    setJob(next);
    if (next.action === 'login' && next.url && props.host && opened !== next.id) {
      opened = next.id;
      openPage(next.url);
    }
  }

  function step(action: SetupAction): Promise<SetupJob> {
    setStage(`${stepTitle(action)}…`);
    return runSetupStep(call(), props.connector, action, onJob, abort?.signal);
  }

  async function run(work: () => Promise<string>) {
    abort?.abort();
    abort = new AbortController();
    setResult(null);
    setJob(null);
    setCode('');
    setCodeError(null);
    try {
      setResult({ ok: true, text: await work() });
    } catch (err) {
      if (!(err instanceof SetupDeclined)) setResult({ ok: false, text: errorText(err) });
    } finally {
      setStage(null);
    }
  }

  const setUp = () =>
    run(async () => {
      const probe = await setUpConnector({
        probe: () => {
          setStage('Checking…');
          return props.probe();
        },
        step,
        confirmInstall: () =>
          confirmDialog({
            title: `Install ${props.title} ${props.where}?`,
            message: `${props.title} isn’t installed ${props.where}. Rovik will run its official installer for this user (no admin rights needed), then sign it in.`,
            confirmLabel: 'Install',
          }),
      });
      return probe.auth?.loggedIn === false
        ? 'Installed, but not signed in yet.'
        : `${props.title} is ready ${props.where}.`;
    });

  const update = () =>
    run(async () => {
      await step('update');
      setStage('Checking…');
      const probe = await props.probe();
      return probe.update
        ? `Still ${probe.version}: ${props.title} didn’t update to ${probe.update}.`
        : `Updated: ${probe.version}.`;
    });

  const signOut = () =>
    run(async () => {
      await step('logout');
      setStage('Checking…');
      await props.probe();
      return 'Signed out.';
    });

  async function sendCode() {
    const j = job();
    if (!j || !code().trim()) return;
    setCodeError(null);
    try {
      setJob(await call()({ op: 'send', id: j.id, text: code() }));
      setCode('');
    } catch (err) {
      setCodeError(errorText(err));
    }
  }

  onMount(() => {
    if (props.autoStart) void setUp();
  });
  // A step left running when the form closes is stopped.
  onCleanup(() => abort?.abort());

  const signingIn = () => {
    const j = job();
    return !!stage() && !!j && !j.done && j.action === 'login' && !!j.url;
  };

  return (
    <div class="connector-setup">
      <div class="provider-test-row connector-setup-row">
        <Show
          when={stage()}
          fallback={
            <>
              <button
                onClick={() => void setUp()}
                title={`Installs ${props.title} if it's missing, tests it and signs it in, ${props.where}`}
              >
                Set up
              </button>
              <Show when={props.update}>
                {(version) => (
                  <button
                    onClick={() => void update()}
                    title={`Updates ${props.title} ${props.where} with its own updater`}
                  >
                    Update to {version()}
                  </button>
                )}
              </Show>
              <Show when={props.signedIn}>
                <button onClick={() => void signOut()}>Sign out</button>
              </Show>
            </>
          }
        >
          {(text) => (
            <>
              <span class="connector-setup-stage" role="status">
                <span class="connector-setup-spinner" aria-hidden="true" />
                {text()}
              </span>
              <button onClick={() => abort?.abort()}>Cancel</button>
            </>
          )}
        </Show>
      </div>

      <Show when={signingIn()}>
        <div class="connector-setup-signin">
          <Show
            when={job()?.deviceCode}
            fallback={
              <>
                <span>
                  {props.host
                    ? 'Sign in on the page opened in your browser, then paste the code it shows.'
                    : 'Sign in in your browser. If it asks you to paste a code here, paste it below.'}
                </span>
                <div class="connector-setup-code">
                  <button onClick={() => openPage(job()!.url!)}>Open sign-in page</button>
                  <input
                    value={code()}
                    onInput={(e) => setCode(e.currentTarget.value)}
                    onKeyDown={(e) => e.key === 'Enter' && void sendCode()}
                    placeholder="Paste the code"
                    aria-label="Sign-in code"
                    spellcheck={false}
                    autocomplete="off"
                  />
                  <button onClick={() => void sendCode()} disabled={!code().trim()}>
                    Send
                  </button>
                </div>
                <Show when={codeError()}>
                  <div class="provider-test-status error">{codeError()}</div>
                </Show>
              </>
            }
          >
            <span>
              {props.host
                ? 'Sign in on the page opened in your browser, and enter there the code printed below.'
                : 'Open the sign-in page, and enter there the code printed below.'}
            </span>
            <div class="connector-setup-code">
              <button onClick={() => openPage(job()!.url!)}>Open sign-in page</button>
            </div>
          </Show>
        </div>
      </Show>

      <Show when={job()}>
        {(j) => (
          <details class="connector-setup-output" open={!!stage() || !!j().error}>
            <summary>
              <code>{j().command}</code>
            </summary>
            <pre>{j().output || '(no output yet)'}</pre>
          </details>
        )}
      </Show>

      <Show when={result()}>
        {(r) => (
          <div
            class="provider-test-status connector-test-status"
            classList={{ ok: r().ok, error: !r().ok }}
          >
            {r().text}
          </div>
        )}
      </Show>
    </div>
  );
}

function openPage(url: string) {
  invoke('open_url', { url }).catch((err) => console.error('Open sign-in page:', err));
}
