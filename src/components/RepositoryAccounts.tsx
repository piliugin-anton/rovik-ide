import { createResource, createSignal, For, onCleanup, Show } from 'solid-js';
import { invoke } from '@tauri-apps/api/core';
import { SettingRow, SettingStatus } from './SettingsControls';

type Provider = 'github' | 'gitlab';
interface Account {
  login: string;
  issuer: string;
}
interface Device {
  challenge: string;
  url: string;
  code: string;
  interval: number;
}
const providers: Provider[] = ['github', 'gitlab'];

function ProviderAccount(props: { provider: Provider }) {
  const call = <T,>(kind: string, fields = {}) =>
    invoke<T>('repository_account', { provider: props.provider, action: { kind, ...fields } });
  const [account, { refetch }] = createResource(() => call<Account | null>('status'));
  const [issuer, setIssuer] = createSignal('');
  const [device, setDevice] = createSignal<Device | null>(null);
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal('');
  let timer: number | undefined;
  let disposed = false;
  onCleanup(() => {
    disposed = true;
    window.clearTimeout(timer);
  });
  async function signIn() {
    setBusy(true);
    setError('');
    const service = issuer().trim();
    try {
      const d = await call<Device>('signIn', { issuer: service });
      if (disposed) return;
      setDevice(d);
      async function poll() {
        try {
          const result = await call<{ pending: boolean }>('poll', {
            issuer: service,
            challenge: d.challenge,
          });
          if (disposed) return;
          if (result.pending) {
            timer = window.setTimeout(poll, Math.max(5, d.interval) * 1000);
            return;
          }
          setDevice(null);
          setBusy(false);
          await refetch();
        } catch (failure) {
          if (!disposed) {
            setDevice(null);
            setBusy(false);
            setError(String(failure));
          }
        }
      }
      timer = window.setTimeout(poll, Math.max(5, d.interval) * 1000);
    } catch (failure) {
      setBusy(false);
      setError(String(failure));
    }
  }
  async function signOut() {
    setBusy(true);
    setError('');
    try {
      await call('signOut');
    } catch (failure) {
      setError(String(failure));
    } finally {
      setBusy(false);
      await refetch();
    }
  }
  const label = props.provider === 'github' ? 'GitHub' : 'GitLab';
  return (
    <section class="settings-section">
      <div class="settings-section-title">{label}</div>
      <Show
        when={!account.error}
        fallback={<SettingStatus warning>{String(account.error)}</SettingStatus>}
      >
        <Show
          when={account()}
          fallback={
            <>
              <SettingRow label="Sign-in service">
                <input
                  type="url"
                  aria-label={label + ' sign-in service'}
                  placeholder="Team hub URL (optional)"
                  value={issuer()}
                  disabled={busy()}
                  onInput={(e) => setIssuer(e.currentTarget.value)}
                />
              </SettingRow>
              <button disabled={busy() || account.loading} onClick={() => void signIn()}>
                Sign in to {label}
              </button>
            </>
          }
        >
          {(a) => (
            <>
              <SettingRow label="Account">{a().login}</SettingRow>
              <Show when={a().issuer}>
                <SettingRow label="Sign-in service">{a().issuer}</SettingRow>
              </Show>
              <button disabled={busy()} onClick={() => void signOut()}>
                Sign out of {label}
              </button>
            </>
          )}
        </Show>
      </Show>
      <Show when={device()}>
        {(d) => (
          <p role="status">
            Open {d().url} and enter <strong>{d().code}</strong> to authorize Rovik.
          </p>
        )}
      </Show>
      <Show when={error()}>
        <SettingStatus warning>{error()}</SettingStatus>
      </Show>
    </section>
  );
}

export default function RepositoryAccounts() {
  return (
    <div class="permissions-settings">
      <p class="settings-hint">
        These accounts are available across your projects. For team memory, enter your team’s
        trusted hub as the sign-in service.
      </p>
      <For each={providers}>{(provider) => <ProviderAccount provider={provider} />}</For>
    </div>
  );
}
