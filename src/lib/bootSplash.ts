let dismissed = false;

/** Fade out and remove the inline boot splash from index.html. Idempotent. */
export function dismissBootSplash(): void {
  if (dismissed) return;
  dismissed = true;
  const el = document.getElementById('boot-splash');
  if (!el) return;
  el.classList.add('done');
  setTimeout(() => el.remove(), 250);
}
