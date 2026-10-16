# End-to-end runs

`run.mjs` launches a built app, clicks and types through its main scenarios
(project, tree, editor, Markdown, search, terminal, Git, settings, the AI
chat against `fake-model.mjs`, restart) and writes `report.md`,
`report.json`, `run.log` and a screenshot per step to `--out`. It exits
with 1 when a step failed.

```sh
node scripts/e2e/run.mjs --app <built binary> --out e2e-out [--only <regex>]
```

It needs `git` and Node 22. Everything else it starts itself: the fake
model (port 18080), and `tauri-driver` (port 4444) unless `--driver bridge`.

## Linux

WebDriver through WebKitWebDriver (`webkit2gtk-driver`), with real X
input when `xdotool` is installed:

```sh
cargo install tauri-driver --locked
npx tauri build
xvfb-run -a -s '-screen 0 1600x1000x24' dbus-run-session -- \
  node scripts/e2e/run.mjs --app src-tauri/target/release/rovik-ide --out e2e-out
```

## Windows

WebDriver through msedgedriver, of the WebView2 runtime's version:

```sh
node scripts/e2e/run.mjs --app src-tauri/target/release/rovik-ide.exe --out e2e-out \
  --native-driver path/to/msedgedriver.exe
```

## macOS

WKWebView has no WebDriver, so the app is built with an in-page bridge
(`bridge-page.js`, added to the build's own copy of the frontend by
`inject-bridge.mjs`) and driven over HTTP on port 17999:

```sh
npx tauri build --bundles app --config scripts/e2e/bridge.tauri.conf.json
node scripts/e2e/run.mjs --driver bridge --out e2e-out \
  --app "src-tauri/target/release/bundle/macos/Rovik IDE.app/Contents/MacOS/rovik-ide"
```

The bridge works on Linux too (`--driver bridge` with a bridge build),
which is how it's checked without a Mac. A bridge build must never ship:
its CSP allows `eval` and a connection to the runner.

## In CI

`.github/workflows/e2e.yml` runs it on one platform at a time: from the
Actions tab (Run workflow), or for a push whose head commit's message has
`[e2e windows]`, `[e2e macos]` or `[e2e linux]`. The report is the job's
summary; the screenshots and logs are its artifact.
