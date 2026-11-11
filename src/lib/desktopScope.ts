import { chatStore } from './chatStore';
import type { EventScope } from './eventReplay';

/** Chats deliberately retained by this window remain followed across project switches. */
export const desktopScope = (): EventScope => ({
  projectId: chatStore.projectId(),
  chats: Object.keys(chatStore.state.open).sort(),
});
