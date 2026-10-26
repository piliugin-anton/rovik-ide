// NewBranchDialog.tsx
//
// "Create a branch for this chat?": asked while a new chat's first run
// waits, the project being on its main branch (lib/chatBranch.ts). The
// branch's name is made up for it and can be changed. Yes creates the
// branch and switches to it, and the dialog stays open, saying why, when
// that fails; No leaves the project where it is. "Don't ask again" turns
// the question off, either way (Settings → Agent runs turns it on again).
// A confirm dialog (ConfirmDialog.tsx) with the fields of the New project
// dialog and the checkbox of Merge or rebase.
//
// Usage: `await askForChatBranch(offer, create)`, with
// <NewBranchDialogHost /> mounted once near the root of the app.
import { createSignal, onCleanup, Show } from 'solid-js';
import { ConfirmDialog } from './ConfirmDialog';
import { branchNameProblem } from '../lib/branchNames';
import type { AskForBranch, BranchOffer } from '../lib/chatBranch';

let nextId = 0;

/** The dialog, controlled: askForChatBranch() shows it through the host. */
export function NewBranchDialog(props: {
  offer: BranchOffer;
  /** Yes: creates the branch and switches to it. A failure is shown, and the dialog stays. */
  create: (name: string) => Promise<void>;
  /** Closed, once the branch was created or with No: whether to ask again. */
  onClose: (askAgain: boolean) => void;
}) {
  const id = `new-branch-${++nextId}`;
  const [name, setName] = createSignal(props.offer.suggested);
  const [askAgain, setAskAgain] = createSignal(true);
  const [creating, setCreating] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);
  const problem = () => branchNameProblem(name());
  const canCreate = () => !creating() && !!name().trim() && !problem();

  async function yes() {
    if (!canCreate()) return;
    setCreating(true);
    setError(null);
    try {
      await props.create(name().trim());
    } catch (err) {
      setError(String(err));
      setCreating(false);
      return;
    }
    props.onClose(askAgain());
  }

  // Not while the branch is being created: the chat would start meanwhile.
  const no = () => {
    if (!creating()) props.onClose(askAgain());
  };

  return (
    <ConfirmDialog
      title="Create a branch for this chat?"
      message={
        <>
          The project is on <strong>{props.offer.current}</strong>. On a new branch, the chat’s
          changes stay off it until you merge them.
        </>
      }
      confirmLabel="Yes"
      cancelLabel="No"
      confirmDisabled={!canCreate()}
      onConfirm={() => void yes()}
      onCancel={no}
    >
      <label class="new-project-label" for={`${id}-name`}>
        Branch name
      </label>
      <input
        id={`${id}-name`}
        class="new-project-input"
        value={name()}
        spellcheck={false}
        autocomplete="off"
        disabled={creating()}
        onInput={(e) => {
          setName(e.currentTarget.value);
          setError(null);
        }}
        onKeyDown={(e) => {
          if (e.key === 'Enter') {
            e.preventDefault();
            void yes();
          }
        }}
      />
      <Show when={problem() ?? error()}>
        {(why) => (
          <div class="new-project-note error" role="alert">
            {why()}
          </div>
        )}
      </Show>
      <label class="pull-strategy-remember">
        <input
          type="checkbox"
          checked={!askAgain()}
          disabled={creating()}
          onChange={(e) => setAskAgain(!e.currentTarget.checked)}
        />
        Don’t ask again
      </label>
    </ConfirmDialog>
  );
}

/*
 * ---------------------------------------------------------------------------
 * Imperative API
 * ---------------------------------------------------------------------------
 */

/** The question being asked, and who waits for its answer. */
interface Asked {
  offer: BranchOffer;
  create: (name: string) => Promise<void>;
  answer: (askAgain: boolean) => void;
}

const [asked, setAsked] = createSignal<Asked | null>(null);

/** Shows the dialog (lib/chatBranch.ts `AskForBranch`): resolves once it's closed. */
export const askForChatBranch: AskForBranch = (offer, create) =>
  new Promise((answer) => {
    // One question at a time: a chat that would be asked meanwhile isn't.
    if (asked()) return answer(true);
    setAsked({ offer, create, answer });
  });

function close(askAgain: boolean) {
  const current = asked();
  setAsked(null);
  current?.answer(askAgain);
}

/** Renders the dialog for askForChatBranch(). Mount once near the app root. */
export function NewBranchDialogHost() {
  // Gone while asking: the chat goes on where the project is.
  onCleanup(() => close(true));

  return (
    <Show when={asked()} keyed>
      {(current) => (
        <NewBranchDialog offer={current.offer} create={current.create} onClose={close} />
      )}
    </Show>
  );
}
