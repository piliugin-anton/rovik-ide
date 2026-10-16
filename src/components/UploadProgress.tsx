// UploadProgress.tsx
//
// A remote send's files going up before its message (6.11), shown above
// the chat's input while they go, from `cloud-event` (lib/cloud.ts). Stop
// cancels the send meanwhile.
import { Show } from 'solid-js';
import { cloud } from '../lib/cloud';
import { formatBytes } from '../lib/textUtils';

export default function UploadProgress(props: { dialogId: string | null }) {
  const upload = () => (props.dialogId ? cloud.uploads[props.dialogId] : undefined);
  return (
    <Show when={upload()}>
      {(u) => (
        <div class="chat-upload-progress" role="status">
          <span class="chat-upload-text">
            Sending files to the server… {formatBytes(u().sent)} of {formatBytes(u().total)}
          </span>
          <span class="chat-upload-bar" aria-hidden="true">
            <span style={{ width: `${Math.round((100 * u().sent) / Math.max(u().total, 1))}%` }} />
          </span>
        </div>
      )}
    </Show>
  );
}
