#!/usr/bin/env node
// The release's notes as GitHub generates them, one line per pull request
// ("* <title> by @<author> in <url>"), with what a title has that GitHub
// would take for a mention kept as code: a title's "@context" (a chat's
// @context files) would be the user "context", whom the release would then
// list among its contributors. The authors stay mentioned.
//
//   gh api -X POST repos/OWNER/REPO/releases/generate-notes \
//     -f tag_name=v1.2.3 --jq .body | node scripts/release-notes.mjs
import { readFileSync } from 'node:fs';
import { pathToFileURL } from 'node:url';

/** A pull request's line: its title, then its author and link. */
const PULL_REQUEST = /^(\* )(.*)( by @[A-Za-z0-9-]+ in https:\/\/\S+)$/gm;

/** What GitHub reads as a mention: a user, or an organization's team. */
const MENTION = /(?<![\w`@])@[A-Za-z0-9][A-Za-z0-9-]*(?:\/[\w.-]+)?/g;

/** `notes` with the mentions in its pull requests' titles as code. */
export function plainMentions(notes) {
  return notes.replace(
    PULL_REQUEST,
    (_, bullet, title, author) => bullet + title.replace(MENTION, '`$&`') + author
  );
}

if (import.meta.url === pathToFileURL(process.argv[1] ?? '').href) {
  process.stdout.write(plainMentions(readFileSync(0, 'utf8')));
}
