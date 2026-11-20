import { createEffect, createUniqueId, onCleanup, untrack } from 'solid-js';
import { dismiss, notify, type NotificationOptions } from './notifications';

/** Update one notification across polls. Dismissing progress hides it until the next transition. */
export function planNotice(state: () => NotificationOptions | undefined) {
  const id = createUniqueId();
  let previous: string | undefined;
  let current: NotificationOptions | undefined;
  createEffect(() => {
    const notice = state();
    current = notice;
    const signature = notice && JSON.stringify(notice);
    if (signature === previous) return;
    previous = signature;
    untrack(() => {
      if (!notice) return dismiss(id);
      notify({
        ...notice,
        id,
        // Polls can refresh an action’s bound run or provider without changing its label.
        actions: notice.actions?.map((action) => ({
          ...action,
          run: () => current?.actions?.find((latest) => latest.label === action.label)?.run(),
        })),
        onDismiss: () => current?.onDismiss?.(),
      });
    });
  });
  onCleanup(() => dismiss(id));
}
