// McpSignIns.tsx
//
// A server's sign-ins on its row in Settings → MCP servers (mcp-plan-2.md,
// Q14): where it's signed in, where it needs a new sign-in for more
// permissions, and Sign in and Sign out. The user's servers and the
// projects' own show them alike (lib/mcpSignIn.ts keeps them).
import { Show } from 'solid-js';
import type { SignIns } from '../lib/mcpSignIn';

/** The row's lines: where server `id` is signed in, and where it wants more permissions. */
export function SignInLines(props: { signIns: SignIns; id: string }) {
  return (
    <>
      <Show when={props.signIns.at(props.id).length > 0}>
        <span class="profile-row-sub">
          Signed in: {props.signIns.said(props.signIns.at(props.id))}
        </span>
      </Show>
      <Show when={props.signIns.wanting(props.id).length > 0}>
        <span class="profile-row-sub">
          Needs more permissions: sign in again for{' '}
          {props.signIns.said(props.signIns.wanting(props.id))}
        </span>
      </Show>
    </>
  );
}

/**
 * The row's Sign in, where the server may ask for one (`offered`) and a
 * place wants it, and Sign out, where it's signed in.
 */
export function SignInActions(props: {
  signIns: SignIns;
  id: string;
  offered: boolean;
  onSignIn: () => void;
  onSignOut: () => void;
}) {
  const busy = () => !!props.signIns.progress();
  return (
    <>
      <Show when={props.offered && props.signIns.missing(props.id).length > 0}>
        <button
          title={`Sign in for ${props.signIns.said(props.signIns.missing(props.id))}, if it asks you to`}
          disabled={busy()}
          onClick={() => props.onSignIn()}
        >
          Sign in
        </button>
      </Show>
      <Show when={props.signIns.at(props.id).length > 0}>
        <button disabled={busy()} onClick={() => props.onSignOut()}>
          Sign out
        </button>
      </Show>
    </>
  );
}
