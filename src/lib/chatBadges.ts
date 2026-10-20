// chatBadges.ts
//
// What the chat list shows on a chat besides its title (U03): "Waiting for
// you" while an approval or a question waits in it or in one of its
// subagents, local or remote; its host's name for a chat on a host, or
// that the host can't be reached; and that it's moving.
import { chatsWaitingOnApprovals } from './approvals';
import { hostName, isWsl, placeLabel, WSL_PAGE, type HostSummary } from './cloud';
import { chatsWaitingOnQuestions } from './questions';
import type { DialogSummary } from '../types';

export interface ChatBadge {
  kind: 'waiting' | 'cloud' | 'offline' | 'moving';
  label: string;
  title: string;
}

/** The chats waiting for the user, by id. */
export function waitingChats(): Record<string, true> {
  return { ...chatsWaitingOnApprovals(), ...chatsWaitingOnQuestions() };
}

/** A chat's badges, in the order they show. */
export function chatBadges(
  dialog: DialogSummary,
  waiting: boolean,
  hosts: HostSummary[]
): ChatBadge[] {
  const badges: ChatBadge[] = [];
  if (waiting) {
    badges.push({
      kind: 'waiting',
      label: 'Waiting for you',
      title: 'An approval or a question waits for you in this chat.',
    });
  }
  const place = dialog.place;
  if (place?.kind === 'host') {
    const name = hostName(place.host, hosts);
    // A chat in WSL is this computer's: no badge while all is well.
    const wsl = isWsl(place.host, hosts);
    switch (hosts.find((h) => h.id === place.host)?.state) {
      case 'offline':
        badges.push({
          kind: 'offline',
          label: `${name} · offline`,
          title: `This chat is on ${name}, which can’t be reached now.`,
        });
        break;
      case 'version-mismatch':
        badges.push({
          kind: 'offline',
          label: `${name} · needs a newer app`,
          title: `${name} serves only newer versions of this app.`,
        });
        break;
      case 'refused':
        badges.push({
          kind: 'offline',
          label: wsl ? 'WSL · couldn’t start' : `${name} · refused`,
          title: wsl
            ? `WSL couldn’t start: ${WSL_PAGE} says why.`
            : `${name} turned this computer away: Settings → Cloud hosts says why.`,
        });
        break;
      default:
        if (!wsl) {
          badges.push({ kind: 'cloud', label: name, title: `This chat runs on ${name}.` });
        }
    }
  } else if (place) {
    badges.push({ kind: 'moving', label: 'Moving…', title: placeLabel(place, hosts).title });
  }
  return badges;
}
