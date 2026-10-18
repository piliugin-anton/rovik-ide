# End-to-end runs

`run.mjs` launches a built app, clicks and types through its main scenarios
(project, tree, editor, Markdown, search, terminal, Git, settings, the AI
chat against `fake-model.mjs`, restart) and writes `report.md`,
`report.json`, `run.log` and a screenshot per step to `--out`. It exits
with 1 when a step failed.

```sh
node scripts/e2e/run.mjs --app <built binary> --out e2e-out [--only <regex>]
```

It needs `git`, Node 22 and TypeScript (`tsc` on PATH: the demo project's
type check is one of the checks an agent's change has to pass before it
lands). Everything else it starts itself: the fake model (port 18080), and
`tauri-driver` (port 4444) unless `--driver bridge`.

The `instances:` steps start other Rovik windows beside the driven one,
with its data: one on the project the driven window has open (which comes
to the front, and the other ends) and one on another project (which only
it may open). They start `--second-app`, by default the driven binary
itself; a bridge build mustn't be one, as its page would take the run's
commands. On Linux under X, "New window" is clicked too, and the claims
the windows it opens hold are checked: only the first has the chats
without a project.

## Linux

WebDriver through WebKitWebDriver (`webkit2gtk-driver`), with real X
input when `xdotool` is installed:

```sh
cargo install tauri-driver --locked
npx tauri build
xvfb-run -a -s '-screen 0 1600x1000x24' dbus-run-session -- \
  node scripts/e2e/run.mjs --app src-tauri/target/release/rovik-ide --out e2e-out
```

An AppImage runs the same way (`--app Rovik.IDE_<version>_amd64.AppImage`),
with one more step: the terminal's shell has the system's environment, not
the AppImage's (no variable naming the AppImage, and nothing it starts as
it logs in fails, as flatpak did with the AppImage's libraries). That step
alone:

```sh
xvfb-run -a -s '-screen 0 1600x1000x24' dbus-run-session -- \
  node scripts/e2e/run.mjs --app Rovik.IDE_0.1.1_amd64.AppImage --out e2e-out \
  --only 'launch|empty state|project: open an existing|appimage'
```

## Windows

WebDriver through msedgedriver, of the WebView2 runtime's version:

```sh
node scripts/e2e/run.mjs --app src-tauri/target/release/rovik-ide.exe --out e2e-out \
  --native-driver path/to/msedgedriver.exe
```

msedgedriver attaches through the debugging port it asks WebView2 for
(`WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS`), which an elevated app's WebView2
ignores (Runtime 150 on): run it as a user, not as administrator (CI, whose
runner is elevated: `gsudo --integrity Medium`). `webview2-probe.ps1 -App
<exe>` shows whether the port opens, and what WebView2 was started with.
Where it doesn't, the bridge (below) drives a Windows build too. Chats are
isolated only with WinFsp installed; without it the agent's run says so and
works in the project itself, which the run notes.

## macOS

WKWebView has no WebDriver, so the app is built with an in-page bridge
(`bridge-page.js`, added to the build's own copy of the frontend by
`inject-bridge.mjs`) and driven over HTTP on port 17999:

```sh
cp -R "src-tauri/target/release/bundle/macos/Rovik IDE.app" /tmp/shipped/
npx tauri build --bundles app --config scripts/e2e/bridge.tauri.conf.json
node scripts/e2e/run.mjs --driver bridge --out e2e-out \
  --app "src-tauri/target/release/bundle/macos/Rovik IDE.app/Contents/MacOS/rovik-ide" \
  --second-app "/tmp/shipped/Rovik IDE.app/Contents/MacOS/rovik-ide"
```

The copy is the build as it ships, kept before the bridge build replaces
it: the other windows of the `instances:` steps (without it, they're
skipped).

The bridge works on Linux too (`--driver bridge` with a bridge build),
which is how it's checked without a Mac. A bridge build must never ship:
its CSP allows `eval` and a connection to the runner.

## In CI

`.github/workflows/e2e.yml` runs it on one platform at a time: from the
Actions tab (Run workflow), or for a push whose head commit's message
starts with `[e2e windows]`, `[e2e macos]` or `[e2e linux]`. The report is
the job's summary; the screenshots and logs are its artifact.
