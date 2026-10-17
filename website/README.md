# rovik-ide.dev

Rovik IDE's website: one page, built with SolidJS, TypeScript and Vite,
prerendered to static HTML and published on GitHub Pages by
`.github/workflows/pages.yml`.

```bash
cd website
npm install
npm run dev        # http://localhost:5173
npm run build      # dist/: the page prerendered, then hydrated in the browser
npm run preview    # serves dist/
npm run typecheck
```

## Downloads

The download buttons ask GitHub's API for the repository's latest release
in the visitor's browser (`src/lib/release.ts`), so a new release shows on
the site as soon as the Release workflow publishes it. Nothing is rebuilt.
The browser keeps the answer for ten minutes: the API allows an address 60
requests an hour without a token.

- Each platform's files are recognized by their extensions, as Tauri names
  them: `.dmg` (Apple silicon or Intel by `aarch64`/`x64` in the name),
  `.exe` and `.msi`, `.AppImage`, `.deb` and `.rpm`. Signatures, updater
  archives and the daemon's builds aren't offered.
- The visitor's system decides which file the main button offers. A Mac
  whose browser doesn't say which chip it has gets Apple silicon.
- Before GitHub answers, without JavaScript, or when GitHub can't be
  reached, the links open the latest release's page. With no release yet,
  they point to the README's build instructions.
- The repository must be public: GitHub doesn't list a private
  repository's releases, or let anyone download them, without signing in.
  To publish the builds from another repository, change `REPO` in
  `src/site.ts`.

## The picture under the hero

Until there's a screenshot, the page draws the app's window in HTML
(`src/components/AppWindow.tsx`). Add `src/assets/screenshot.png` (or
`.jpg`, `.webp`, `.avif`) and the page shows it instead. A good one is
about 2560 × 1600, with a project open, code in the editor and the AI Chat
tab beside it.

## Icons

`scripts/icons.mjs` draws the icon and writes every size of it: the app's
(`src-tauri/icons/`: PNGs, `icon.ico`, `icon.icns`) and the site's
(`public/favicon.svg`, `favicon.ico`, `apple-touch-icon.png`, and
`src/assets/icon.svg`). After changing the drawing, run `npm run icons`.

The link preview, `public/og.png`, is a screenshot of `scripts/og.html`;
the command that redraws it is at the top of that file.

## Publishing on rovik-ide.dev

Once, in the repository's **Settings → Pages**:

1. Set **Source** to **GitHub Actions**. The next run of the Website
   workflow (a push to master that changes `website/`, or **Run workflow**
   in the Actions tab) publishes the site.
2. Set **Custom domain** to `rovik-ide.dev`, then tick **Enforce HTTPS**
   once GitHub has made the certificate. A `.dev` domain only works over
   HTTPS, so the site shows only from then on.

At the domain's DNS provider:

| Name            | Type  | Value                                                                                      |
| --------------- | ----- | ------------------------------------------------------------------------------------------ |
| `rovik-ide.dev` | A     | `185.199.108.153`, `185.199.109.153`, `185.199.110.153`, `185.199.111.153`                 |
| `rovik-ide.dev` | AAAA  | `2606:50c0:8000::153`, `2606:50c0:8001::153`, `2606:50c0:8002::153`, `2606:50c0:8003::153` |
| `www`           | CNAME | `piliugin-anton.github.io`                                                                 |

Verifying the domain in your GitHub account's **Settings → Pages** keeps
anyone else's repository from publishing on it.
