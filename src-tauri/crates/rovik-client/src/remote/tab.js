// Rovik's script in a page a browser tab of the app shows: the tab port
// (tab_port.rs) puts its tag after each page it loads into the tab, and
// answers it at /__rovik/tab.js. It tells the tab where the page is, as it
// navigates, so that the tab's address bar follows the page and Reload
// reloads it there (src/components/BrowserView.tsx). It reports the
// page's own address and nothing else.
(() => {
  // Gone from the page at once: a page that hydrates its whole document
  // (React's hydrateRoot) finds nothing of Rovik's in it.
  document.currentScript?.remove();
  // Only in the tab's own frame: not in a frame of the page's (its
  // parent is the page, whose messages are its own), nor in a window of
  // its own.
  const key = Symbol.for('rovik.tab');
  if (window.parent === window || window.parent !== window.top || window[key]) return;
  window[key] = true;
  let told = '';
  const tell = () => {
    if (location.href === told) return;
    told = location.href;
    window.parent.postMessage({ type: 'rovik-tab-location', href: told }, '*');
  };
  addEventListener('popstate', tell);
  addEventListener('hashchange', tell);
  addEventListener('pageshow', tell);
  // history.pushState and replaceState fire no event: the address is
  // looked at a few times a second instead of patching them.
  setInterval(tell, 250);
  tell();
})();
