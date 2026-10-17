/* A "copy" button on every code block. The button sits in a wrapper around the
   <pre> (not inside it, which scrolls), and one delegated listener serves every
   button, including the ones in the clone of a card that lamp.js builds. */
(() => {
  const label = 'Copy';
  for (const pre of document.querySelectorAll('.node-body pre')) {
    if (pre.parentElement.classList.contains('code-wrap')) continue;
    const wrap = document.createElement('div');
    wrap.className = 'code-wrap';
    pre.replaceWith(wrap);
    const btn = document.createElement('button');
    btn.type = 'button';
    btn.className = 'copy-btn';
    btn.textContent = label;
    btn.setAttribute('aria-label', 'Copy code to the clipboard');
    wrap.append(pre, btn);
  }

  async function copy(text) {
    try {
      await navigator.clipboard.writeText(text);
      return true;
    } catch (_) {
      // No clipboard API (an insecure context, an old browser): select and copy.
      const area = document.createElement('textarea');
      area.value = text;
      area.setAttribute('readonly', '');
      area.style.cssText = 'position:fixed;top:0;left:0;opacity:0';
      document.body.appendChild(area);
      area.select();
      let ok = false;
      try { ok = document.execCommand('copy'); } catch (_) {}
      area.remove();
      return ok;
    }
  }

  const timers = new WeakMap();
  document.addEventListener('click', async event => {
    const btn = event.target.closest('.copy-btn');
    if (!btn) return;
    event.preventDefault();
    event.stopPropagation();
    const pre = btn.parentElement.querySelector('pre');
    const text = (pre.querySelector('code') || pre).textContent.replace(/\n$/, '');
    const ok = await copy(text);
    btn.textContent = ok ? 'Copied' : 'Failed';
    btn.classList.toggle('done', ok);
    clearTimeout(timers.get(btn));
    timers.set(btn, setTimeout(() => { btn.textContent = label; btn.classList.remove('done'); }, 1600));
  });
})();
