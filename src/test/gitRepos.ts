// The Git panel's repositories in component tests: GitPanel.tsx reads them
// with `git_repos` (src-tauri/src/git/repos.rs) before it shows a section.
import type { GitRepos } from '../types';

/** `git_repos`' answer for a window whose one folder is a repository of its own. */
export function oneRepository(path: string): GitRepos {
  return {
    repos: [
      {
        key: `${path}/.git`,
        workdir: path,
        name: path.split('/').pop() ?? path,
        scopes: [''],
        folders: [path],
      },
    ],
    without: [],
  };
}
