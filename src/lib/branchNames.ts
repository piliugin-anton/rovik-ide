// branchNames.ts
//
// Git branch names: what makes one valid, and a fresh one to suggest. The
// Git panel checks the names typed into it here, and a new chat's offer of
// a branch of its own (lib/chatBranch.ts) suggests one made up of an
// adjective and a programmer's last name, "kind-torvalds", that no branch
// of the repository has yet.

/**
 * Why `name` can't be a branch's name, or null when it can (or is empty):
 * the obvious mistakes, by the rules of `git check-ref-format --branch`.
 * The backend checks again, for real (src-tauri/src/git/branches.rs,
 * `validate_branch_name`).
 */
export function branchNameProblem(name: string): string | null {
  const n = name.trim();
  if (!n) return null;
  if (
    n === 'HEAD' ||
    n === '@' ||
    n.startsWith('-') ||
    n.startsWith('/') ||
    n.endsWith('/') ||
    n.endsWith('.') ||
    n.includes('..') ||
    n.includes('//') ||
    n.includes('@{') ||
    // No path component may start with "." or end with ".lock".
    n.split('/').some((part) => part.startsWith('.') || part.endsWith('.lock')) ||
    /[\s~^:?*\[\\\x00-\x1f\x7f]/.test(n)
  ) {
    return `'${n}' isn't a valid branch name.`;
  }
  return null;
}

/** A list of words, written a few to a line. */
const words = (text: string): readonly string[] => text.trim().split(/\s+/);

/** The first half of a suggested name: a friendly adjective. */
export const ADJECTIVES = words(`
  agile amazing amiable ardent awesome blissful bold brave brilliant busy calm careful charming
  cheerful clever cool cosmic cozy crisp curious daring dapper dazzling determined diligent dreamy
  eager elegant eloquent epic fearless festive fluent focused friendly frosty gallant generous
  gentle gifted gleaming golden graceful gracious happy hardy helpful heroic honest hopeful humble
  inspired jolly jovial joyful keen kind laughing lively loyal lucid lucky magical majestic mellow
  merry mighty mindful modest nifty nimble noble optimistic patient peaceful plucky polite
  practical proud quick quiet quirky radiant relaxed resolute serene sharp sincere sleek smart
  snappy spirited steady stellar sturdy sunny swift tender thoughtful tidy tranquil trusty upbeat
  valiant vibrant vigilant vivid warm whimsical wise witty wonderful zealous zen zesty
`);

/**
 * The second half: programmers' last names, from Wikipedia's list of
 * programmers (https://en.wikipedia.org/wiki/List_of_programmers),
 * lowercase, with what a branch name can't hold, or reads badly in, left
 * out: accents, spaces, hyphens and dots ("Guy L. Steele Jr." is steele,
 * "Tim Berners-Lee" bernerslee, "Guido van Rossum" vanrossum, "Tatu
 * Ylönen" ylonen).
 */
export const PROGRAMMERS = words(`
  abrash adams aho alexandrescu allen allman andreessen armstrong atkinson augustsson
  backus bak barnaby bartle beck behlendorf bellard bemer bernerslee bernstein bina blank bloch
  booth bos bourne bradley bricklin bright brin budge buytaert
  capps carmack clark cohen cooper cowlishaw cox crockford cunningham cutler
  dahl davidson davis dean deicaza deraadt deutsch dijkstra dillon dorsey dougiamas duff
  earnest eich ershov
  fahlman farmer fitzpatrick fox frankel frankston furber
  gamma garriott gates gavin ghemawat gilbert goldberg gordon gosling gosper graham griesemer
  griswold
  hamilton hansson hejlsberg hertzfeld hevery hewitt hickey hoare hopper hyatt
  ichbiah ierusalimschy ingalls ivarsoy iverson iwatani
  johnson jolitz joy
  kamp kapor katz kay kemeny kernighan kildall knuth koenig kolling korn koza kurtz
  lamport lampson landin lane lantinga lattner lerdorf lesk liedtke liskov lovelace
  matsumoto mccarthy mcclanahan mcilroy mckusick mechner meier meyer minter moolenaar moon moore
  morris mullenweg murdock muuss
  naughton naur norton nygaard
  odersky oikarinen ousterhout ozzie
  page pajitnov papert paterson persson petzold pike pitman pittman poettering
  raskin raymond reid resig richards ritchie rivest ross russell russinovich
  sammet sawyer scheifler schlueter simonyi skrenta spolsky stallman steele stepanov stonebraker
  stroustrup sussman sutherland swartz sweeney
  tallinn tanenbaum tang tesler tevanian thompson tiemann torvalds tridgell trubshaw
  vanrossum venema vixie volkerding
  walker wall wallace warnock watson weizenbaum wheeler whitney wilson wirth wolfram wozniak
  ylonen
  zawinski zimmermann zuckerberg zuse
`);

/**
 * A name for a new branch that none of `taken` has: an adjective and a
 * programmer's last name, "kind-torvalds". With so many pairs, one is
 * rarely taken; one that is gets a number, "kind-torvalds-2".
 */
export function suggestBranchName(
  taken: Iterable<string>,
  random: () => number = Math.random
): string {
  const used = new Set(taken);
  const pick = (list: readonly string[]) => list[Math.floor(random() * list.length)];
  const pair = `${pick(ADJECTIVES)}-${pick(PROGRAMMERS)}`;
  let name = pair;
  for (let n = 2; used.has(name); n++) name = `${pair}-${n}`;
  return name;
}
