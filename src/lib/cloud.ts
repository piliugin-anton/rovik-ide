// The hosts' own events on `cloud-event`, apart from the chats' (the app's
// `CloudEvent`, rovik-client): a message's files going up to its host, a
// project being pushed there, and a remote chat's changes looking different
// after its result came in or landed, or its project synced.

import { listen } from '@tauri-apps/api/event';

export type CloudEvent =
  | { type: 'uploading'; dialogId: string; messageId: string; sent: number; total: number }
  | { type: 'pushing'; dialogId: string; project: string; sent: number }
  | { type: 'changed'; dialogId: string };

/** Whether the isolation bar of chat `dialogId` should look again after `event`. */
export function barChanged(event: CloudEvent, dialogId: string | null | undefined): boolean {
  return event.type === 'changed' && !!dialogId && event.dialogId === dialogId;
}

/** Calls `handler` with each cloud event, until the function it returns is called. */
export function onCloudEvent(handler: (event: CloudEvent) => void): () => void {
  const listening = listen<CloudEvent>('cloud-event', (e) => handler(e.payload));
  return () => void listening.then((stop) => stop());
}
