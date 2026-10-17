// PrepareRecord.tsx
//
// An Autopilot task's Prepare stage in the chat (S03, agent_run/prepare.rs):
// the installs its lockfiles imply, run before the code is written, each
// with how it went. Updates live while they run.
import { prepareHeadline, type PrepareMeta } from '../lib/verification';
import { InstallList } from './VerificationRecord';

export default function PrepareRecord(props: {
  meta: PrepareMeta;
  /** Its run is running right now. */
  live: boolean;
}) {
  /** Saved as running, but nothing runs it: the app closed mid-install. */
  const interrupted = () => props.meta.status === 'running' && !props.live;
  return (
    <div class={`chat-verify ${interrupted() ? 'stopped' : props.meta.status}`} role="status">
      <div class="chat-verify-head">
        <span class="chat-verify-title">{prepareHeadline(props.meta, interrupted())}</span>
      </div>
      <InstallList installs={props.meta.installs} />
    </div>
  );
}
