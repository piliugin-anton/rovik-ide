// Where the page points. The downloads come from this repository's latest
// GitHub release, looked up in the visitor's browser (lib/release.ts).
export const REPO = 'piliugin-anton/rovik-ide';

export const REPO_URL = `https://github.com/${REPO}`;
export const RELEASES_URL = `${REPO_URL}/releases`;
export const LATEST_RELEASE_URL = `${REPO_URL}/releases/latest`;
export const ISSUES_URL = `${REPO_URL}/issues`;
export const LATEST_RELEASE_API = `https://api.github.com/repos/${REPO}/releases/latest`;

/** The README's build instructions: all of them, or one platform's. */
export function buildFromSourceUrl(platform?: 'linux' | 'macos' | 'windows'): string {
  return `${REPO_URL}#${platform ?? 'installing-and-running'}`;
}
