// CommandSuggestions.tsx
//
// The composer's `/` picker for a connector's commands
// (lib/connectorCommands.ts): each command's name, its aliases, what it
// takes and what it does, in the list the `@` file picker uses. The name
// is what the user picks, so it stays whole: the hint after it is cut
// short first, then the aliases (styles.css). The composer keeps focus
// and handles the keys; a click picks a row.
import { Index, Show } from 'solid-js';
import type { ConnectorCommand } from '../lib/connectorCommands';

export interface CommandSuggestionsProps {
  /** The commands matching what's typed, best first. */
  commands: ConnectorCommand[];
  /** The row Enter or Tab picks. */
  selected: number;
  /** A line above the rows: the listing is on its way, or why it failed. */
  status?: string | null;
  onPick: (command: ConnectorCommand) => void;
}

export default function CommandSuggestions(props: CommandSuggestionsProps) {
  return (
    <div class="file-suggestions command-suggestions" role="listbox" aria-label="Commands">
      <Show when={props.status}>
        <div class="command-suggestions-status" role="status">
          {props.status}
        </div>
      </Show>
      <Index each={props.commands}>
        {(command, index) => (
          <button
            type="button"
            class="file-suggestion"
            classList={{ selected: index === props.selected }}
            role="option"
            aria-selected={index === props.selected}
            // Never focusable: the input keeps focus and its caret.
            tabIndex={-1}
            onMouseDown={(e) => e.preventDefault()}
            onClick={() => props.onPick(command())}
          >
            <span class="file-suggestion-head">
              <span class="file-suggestion-name">/{command().name}</span>
              {/* The aliases before the hint: the hint takes only the room
                  left, so a long one can't push them out. */}
              <Show when={(command().aliases ?? []).length > 0}>
                <span class="file-suggestion-tag">
                  {(command().aliases ?? []).map((a) => `/${a}`).join(' ')}
                </span>
              </Show>
              <Show when={command().argumentHint}>
                <span class="command-suggestion-hint">{command().argumentHint}</span>
              </Show>
            </span>
            <Show when={command().description}>
              <span class="file-suggestion-path">{command().description}</span>
            </Show>
          </button>
        )}
      </Index>
    </div>
  );
}
