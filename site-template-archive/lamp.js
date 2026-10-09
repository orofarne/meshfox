/* "Under the lamp": any paper can be lifted off the board and read full
   screen. The sheet is built from a clone of the card (title, tags, body), so
   the board, its layout and the threads are never touched. */
(() => {
  const reduced = () => matchMedia('(prefers-reduced-motion:reduce)').matches;
  const cards = [...document.querySelectorAll('.node')].filter(card => !card.classList.contains('type-group'));
  if (!cards.length) return;

  const dialog = document.createElement('dialog');
  dialog.className = 'lamp';
  dialog.innerHTML =
    '<div class="lamp-dim"></div>' +
    '<div class="lamp-stage"><div class="lamp-shadow"><article class="lamp-sheet">' +
    '<button type="button" class="lamp-close" aria-label="Close" title="Close (Esc)">✕</button>' +
    '<header class="lamp-head"><nav class="lamp-crumbs" aria-label="Path"></nav>' +
    '<h2 class="lamp-title"></h2><div class="lamp-tags"></div></header>' +
    '<div class="lamp-scroll"></div>' +
    '</article></div></div>';
  document.body.appendChild(dialog);
  const sheet = dialog.querySelector('.lamp-sheet');
  const crumbs = dialog.querySelector('.lamp-crumbs');
  const title = dialog.querySelector('.lamp-title');
  const tags = dialog.querySelector('.lamp-tags');
  const scroll = dialog.querySelector('.lamp-scroll');
  const stage = dialog.querySelector('.lamp-stage');
  const dim = dialog.querySelector('.lamp-dim');

  let current = null, opener = null, closing = false, hovered = null;

  const titleOf = card => card.querySelector(':scope > .node-title .node-title-text')?.textContent.trim() || '';
  function parentCard(card) {
    const row = card.closest('.node-row')?.parentElement?.closest('.node-row');
    return row?.querySelector(':scope > .node') || null;
  }
  const isExpandable = card => card && cards.includes(card);

  function fill(card) {
    current = card;
    sheet.style.setProperty('--accent', getComputedStyle(card).getPropertyValue('--accent') || '');
    title.textContent = titleOf(card);

    const path = [];
    for (let p = parentCard(card); p; p = parentCard(p)) path.unshift(p);
    crumbs.replaceChildren();
    path.forEach((p, i) => {
      if (i) crumbs.append(' › ');
      if (isExpandable(p)) {
        const a = document.createElement('a');
        a.href = '#' + p.id;
        a.textContent = titleOf(p);
        a.addEventListener('click', event => { event.preventDefault(); open(p); });
        crumbs.append(a);
      } else {
        const s = document.createElement('span');
        s.textContent = titleOf(p);
        crumbs.append(s);
      }
    });
    crumbs.hidden = !path.length;

    tags.replaceChildren();
    const src = card.querySelector(':scope > .node-tags');
    if (src) tags.append(...[...src.childNodes].map(n => n.cloneNode(true)));
    tags.hidden = !src;

    const body = card.querySelector(':scope > .node-body');
    scroll.replaceChildren();
    if (body) {
      const copy = body.cloneNode(true);
      copy.classList.remove('long');
      copy.removeAttribute('style');
      copy.removeAttribute('id');
      for (const el of copy.querySelectorAll('[id]')) el.removeAttribute('id');
      scroll.append(copy);
    }
    scroll.scrollTop = 0;
  }

  function setHash(card) {
    try { history.replaceState(null, '', card ? '#' + card.id : location.pathname + location.search); } catch (_) {}
  }

  function open(card, from) {
    if (closing) return;
    const wasOpen = dialog.open;
    fill(card);
    setHash(card);
    if (wasOpen) return;
    opener = from || card.querySelector('.lamp-btn');
    document.documentElement.classList.add('lamp-on');
    dialog.showModal();
    scroll.focus({ preventScroll: true });
    if (reduced()) return;
    dim.animate({ opacity: [0, 1] }, { duration: 320, easing: 'ease-out' });
    const to = sheet.getBoundingClientRect();
    const fromBox = card.getBoundingClientRect();
    const visible = fromBox.width > 0 && fromBox.bottom > 0 && fromBox.top < innerHeight && fromBox.right > 0 && fromBox.left < innerWidth;
    if (visible) {
      const s = Math.min(1, fromBox.width / to.width);
      sheet.animate([
        { transformOrigin: '0 0', transform: `translate(${fromBox.left - to.left}px,${fromBox.top - to.top}px) scale(${s})`, opacity: .6 },
        { transformOrigin: '0 0', transform: 'none', opacity: 1 },
      ], { duration: 360, easing: 'cubic-bezier(.2,.8,.2,1)' });
    } else {
      sheet.animate({ opacity: [0, 1], transform: ['translateY(24px)', 'none'] }, { duration: 260, easing: 'ease-out' });
    }
  }

  function finish() {
    dialog.close();
    for (const el of [dim, sheet]) el.getAnimations().forEach(a => a.cancel());
    document.documentElement.classList.remove('lamp-on');
    closing = false;
    const back = opener && opener.isConnected ? opener : null;
    current = null; opener = null;
    setHash(null);
    back?.focus({ preventScroll: true });
  }

  function close() {
    if (!dialog.open || closing) return;
    closing = true;
    if (reduced()) { finish(); return; }
    const card = current;
    const to = card.getBoundingClientRect();
    const fromBox = sheet.getBoundingClientRect();
    const visible = to.width > 0 && to.bottom > 0 && to.top < innerHeight && to.right > 0 && to.left < innerWidth;
    dim.animate({ opacity: [1, 0] }, { duration: 280, easing: 'ease-in', fill: 'forwards' });
    const frames = visible
      ? [{ transformOrigin: '0 0', transform: 'none', opacity: 1 },
         { transformOrigin: '0 0', transform: `translate(${to.left - fromBox.left}px,${to.top - fromBox.top}px) scale(${Math.min(1, to.width / fromBox.width)})`, opacity: .5 }]
      : [{ opacity: 1, transform: 'none' }, { opacity: 0, transform: 'translateY(24px)' }];
    sheet.animate(frames, { duration: 280, easing: 'cubic-bezier(.5,0,.8,.4)', fill: 'forwards' }).onfinish = finish;
  }

  // Expand stamp on every paper, inside the title so a folded paper keeps it.
  for (const card of cards) {
    const head = card.querySelector(':scope > .node-title');
    if (!head) continue;
    const btn = document.createElement('button');
    btn.type = 'button';
    btn.className = 'lamp-btn';
    btn.textContent = '⤢';
    btn.title = 'Read full screen (F)';
    btn.setAttribute('aria-label', 'Read full screen');
    head.appendChild(btn);
    btn.addEventListener('click', event => {
      event.preventDefault();
      event.stopPropagation();
      open(card, btn);
    });
    // The title is a <summary>; stop its keyboard toggle when the expand button is focused.
    btn.addEventListener('keydown', event => event.stopPropagation());
    head.addEventListener('dblclick', event => {
      if (event.target.closest('.lamp-btn')) return;
      getSelection()?.removeAllRanges();
      open(card, btn);
    });
  }

  document.addEventListener('pointerover', event => { hovered = event.target.closest?.('.node') || null; });
  document.addEventListener('keydown', event => {
    if (event.key !== 'f' && event.key !== 'F') return;
    if (event.ctrlKey || event.metaKey || event.altKey || dialog.open) return;
    if (event.target.closest?.('input,textarea,select,[contenteditable]')) return;
    const focused = document.activeElement?.closest?.('.node');
    const card = [focused, hovered].find(isExpandable);
    if (!card) return;
    event.preventDefault();
    open(card, card.querySelector('.lamp-btn'));
  });

  dialog.addEventListener('cancel', event => { event.preventDefault(); close(); });
  dialog.addEventListener('click', event => {
    if (event.target === dialog || event.target === stage || event.target === dim) close();
  });
  dialog.querySelector('.lamp-close').addEventListener('click', close);
  // Links in the path (or a body link to another paper) swap the sheet in place.
  window.addEventListener('hashchange', () => {
    const card = document.getElementById(location.hash.slice(1));
    if (isExpandable(card)) open(card);
  });

  const initial = location.hash.length > 1 && document.getElementById(decodeURIComponent(location.hash.slice(1)));
  if (isExpandable(initial)) open(initial);
})();
