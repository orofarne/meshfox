/* A spatial group's children carry authored coordinates and dimensions.
   Size its local cork field from those boxes before arrows are measured. */
(() => {
  const pinClearance=14;
  for (const board of document.querySelectorAll('.group-canvas')) {
    let right=360, bottom=180;
    for (const row of board.querySelectorAll(':scope > .node-row.spatial-item')) {
      const card=row.querySelector(':scope > .node');
      right=Math.max(right,parseFloat(row.style.left)+parseFloat(card.style.width)+28);
      const top=parseFloat(row.style.top)+pinClearance;
      row.style.top=top+'px';
      bottom=Math.max(bottom,top+parseFloat(card.style.height)+28);
    }
    board.style.width=right+'px';
    board.style.height=bottom+'px';
  }
  // Fixed-size papers keep their pins outside the clipping area, while
  // their prose scrolls within the actual space left below the title.
  function fitBodies() {
    for (const card of document.querySelectorAll('.node-row.authored-item > .node, .group-canvas > .node-row.spatial-item > .node')) {
      const body=card.querySelector(':scope > .node-body');
      if (!body || !body.getClientRects().length) continue;
      const bottom=card.getBoundingClientRect().bottom;
      const top=body.getBoundingClientRect().top;
      const padding=parseFloat(getComputedStyle(card).paddingBottom) || 0;
      body.style.maxHeight=Math.max(0,bottom-top-padding)+'px';
    }
  }
  // An ordinary body flows at its natural height; only one far taller than
  // the screen is capped (.long), so tall prose does not shove its siblings
  // off the page. Measured uncapped, synchronously, before arrows redraw.
  function markLong() {
    for (const body of document.querySelectorAll('.node-row:not(.authored-item):not(.spatial-item) > details.node[open] > .node-body')) {
      body.classList.remove('long');
      if (body.scrollHeight > innerHeight*3) body.classList.add('long');
    }
  }
  markLong(); fitBodies();
  window.addEventListener('load',() => { markLong(); fitBodies(); });
  window.addEventListener('resize',() => { markLong(); fitBodies(); });
  document.addEventListener('toggle',() => { markLong(); requestAnimationFrame(fitBodies); },true);
  // One-line code blocks (an install command) wrap instead of scrolling
  // sideways inside a board that already scrolls sideways.
  for (const pre of document.querySelectorAll('.node-body pre')) {
    if (!pre.textContent.trim().includes('\n')) pre.classList.add('oneline');
  }

  // Like the web canvas, opening a subtree that extends past the right edge
  // brings its parent paper near the left edge, leaving room for the reveal.
  const viewport=document.querySelector('.board-wrap');
  document.addEventListener('click',event => {
    const title=event.target.closest('summary.node-title');
    const card=title?.parentElement;
    if (!viewport || !card?.classList.contains('node') || card.open) return;
    requestAnimationFrame(() => requestAnimationFrame(() => {
      if (!card.open) return;
      const row=card.closest('.node-row');
      const bounds=viewport.getBoundingClientRect();
      const cardBox=card.getBoundingClientRect();
      let right=cardBox.right;
      for (const child of row.querySelectorAll('.node')) {
        let hidden=false;
        for (let parent=child.parentElement; parent && parent!==row; parent=parent.parentElement) {
          if (parent.matches('details:not([open])')) { hidden=true; break; }
        }
        if (!hidden && child.getClientRects().length) right=Math.max(right,child.getBoundingClientRect().right);
      }
      if (cardBox.left >= bounds.left+16 && right <= bounds.right-16) return;
      viewport.scrollTo({left:viewport.scrollLeft+cardBox.left-bounds.left-24,behavior:matchMedia('(prefers-reduced-motion:reduce)').matches?'auto':'smooth'});
    }));
  });
})();
