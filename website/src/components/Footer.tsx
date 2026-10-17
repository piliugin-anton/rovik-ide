import iconUrl from '../assets/icon.svg';
import { buildFromSourceUrl, ISSUES_URL, RELEASES_URL, REPO_URL } from '../site';

export default function Footer() {
  return (
    <footer class="footer">
      <div class="container footer-inner">
        <a class="brand" href="#top">
          <img src={iconUrl} width="24" height="24" alt="" />
          <span>Rovik IDE</span>
        </a>
        <nav class="footer-links" aria-label="Project">
          <a href={REPO_URL}>GitHub</a>
          <a href={RELEASES_URL}>Releases</a>
          <a href={ISSUES_URL}>Issues</a>
          <a href={buildFromSourceUrl()}>Build from source</a>
        </nav>
        <p class="footer-note">Made with Tauri, Rust and SolidJS.</p>
      </div>
    </footer>
  );
}
