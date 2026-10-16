// morphHtml.ts
//
// Puts new HTML into an element by changing only what differs from what's
// there, so what stays the same stays the same nodes. A reply streaming
// into the chat had its whole text replaced on every token: a click on a
// link in it was lost (the link under the pointer was replaced between
// press and release), and so was any text selected in it.

/** Makes `target`'s content `html`, keeping the nodes that are already equal. */
export function morphHtml(target: Element, html: string) {
  const template = document.createElement('template');
  template.innerHTML = html;
  morphChildren(target, template.content);
}

function morphChildren(target: Node, source: Node) {
  const olds = [...target.childNodes];
  const news = [...source.childNodes];
  news.forEach((next, i) => {
    const old = olds[i];
    if (!old) target.appendChild(next);
    else if (old.isEqualNode(next)) return;
    else if (old.nodeType === Node.TEXT_NODE && next.nodeType === Node.TEXT_NODE)
      old.nodeValue = next.nodeValue;
    else if (
      old.nodeType === Node.ELEMENT_NODE &&
      next.nodeType === Node.ELEMENT_NODE &&
      old.nodeName === next.nodeName
    ) {
      syncAttributes(old as Element, next as Element);
      morphChildren(old, next);
    } else target.replaceChild(next, old);
  });
  for (const old of olds.slice(news.length)) old.remove();
}

function syncAttributes(old: Element, next: Element) {
  for (const { name } of [...old.attributes]) {
    // A <details> the user opened stays open.
    if (name === 'open' && old.nodeName === 'DETAILS') continue;
    if (!next.hasAttribute(name)) old.removeAttribute(name);
  }
  for (const { name, value } of [...next.attributes]) {
    if (old.getAttribute(name) !== value) old.setAttribute(name, value);
  }
}
