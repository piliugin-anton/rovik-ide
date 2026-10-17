import iconUrl from '../assets/icon.svg';
import { REPO_URL } from '../site';
import { GitHubLogo } from './icons';

export default function Header() {
  return (
    <header class="header">
      <div class="container header-inner">
        <a class="brand" href="#top">
          <img src={iconUrl} width="28" height="28" alt="" />
          <span>Rovik IDE</span>
        </a>
        <nav class="nav" aria-label="Main">
          <a class="nav-features" href="#features">
            Features
          </a>
          <a href="#download">Download</a>
          <a class="nav-github" href={REPO_URL}>
            <GitHubLogo size={18} />
            <span>GitHub</span>
          </a>
        </nav>
      </div>
    </header>
  );
}
