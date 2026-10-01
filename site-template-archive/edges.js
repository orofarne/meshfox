/* Taut threads join pin centres. This theme intentionally ignores authored routes. */
(() => {
  const data = document.getElementById('mesh-edge-data');
  if (!data) return;
  const edges = JSON.parse(data.textContent);
  const tree = document.querySelector('.tree');
  if (!tree) return;
  const allTree = tree.classList.contains('all-svg-edges');
  const ns = 'http://www.w3.org/2000/svg';
  const svgEl = name => document.createElementNS(ns, name);
  const box = (el, origin) => {
    const a = el.getBoundingClientRect(), b = origin.getBoundingClientRect();
    return { left: a.left - b.left, top: a.top - b.top, right: a.right - b.left,
      bottom: a.bottom - b.top, cx: (a.left + a.right) / 2 - b.left,
      cy: (a.top + a.bottom) / 2 - b.top };
  };
  // .node::before is centred horizontally and its 14px pin starts 8px
  // above the paper, putting the pin centre 1px above its top edge.
  const pin = b => ({x:b.cx,y:b.top-1});
  const visible = el => {
    for (let parent=el.parentElement; parent; parent=parent.parentElement) {
      if (parent.matches('details:not([open])')) return false;
    }
    return true;
  };
  function draw() {
    tree.querySelectorAll('.mesh-edge-overlay, .mesh-edge-label-overlay').forEach(el => el.remove());
    const layers = new Map();
    const layer = origin => {
      if (layers.has(origin)) return layers.get(origin);
      const overlay=svgEl('svg'), labels=svgEl('svg'), defs=svgEl('defs');
      overlay.classList.add('mesh-edge-overlay'); labels.classList.add('mesh-edge-label-overlay');
      const width=Math.max(origin.scrollWidth,origin.clientWidth);
      const height=Math.max(origin.scrollHeight,origin.clientHeight);
      for (const svg of [overlay,labels]) { svg.setAttribute('width',width); svg.setAttribute('height',height); }
      overlay.appendChild(defs);
      const value={overlay,labels,defs}; layers.set(origin,value); return value;
    };
    const defaultColor = getComputedStyle(document.documentElement).getPropertyValue('--accent-default').trim() || '#9b4437';
    edges.forEach((edge, i) => {
      if (edge.kind === 'tree' && !allTree && !edge.via.length && !edge.source_side && !edge.target_side && !edge.label) return;
      const from = document.getElementById('node-' + edge.from);
      const to = document.getElementById('node-' + edge.to);
      if (!from || !to || !visible(from) || !visible(to) || !from.getClientRects().length || !to.getClientRects().length) return;
      const fromBoard=from.closest('.group-canvas'), toBoard=to.closest('.group-canvas');
      const origin=fromBoard && fromBoard===toBoard ? fromBoard : tree;
      const {overlay,labels,defs}=layer(origin);
      const a=box(from,origin), b=box(to,origin);
      const start=pin(a), end=pin(b);
      const path=svgEl('path');
      path.setAttribute('d',`M${start.x},${start.y} L${end.x},${end.y}`);
      path.setAttribute('fill','none');
      const color=edge.color || defaultColor;
      path.setAttribute('stroke',color);
      path.setAttribute('stroke-width',edge.kind==='tree' ? '1.7':'2');
      if (edge.style==='dashed') path.setAttribute('stroke-dasharray','7 5');
      if (edge.style==='dotted') path.setAttribute('stroke-dasharray','2 5');
      for (const endName of ['start','end']) {
        if (!edge['arrow_'+endName]) continue;
        const id='mesh-arrow-'+i+'-'+endName, marker=svgEl('marker'), head=svgEl('path');
        marker.setAttribute('id',id); marker.setAttribute('viewBox','0 0 10 10');
        marker.setAttribute('markerWidth','7'); marker.setAttribute('markerHeight','7');
        marker.setAttribute('refX',endName==='start'?'1':'9'); marker.setAttribute('refY','5');
        marker.setAttribute('orient','auto-start-reverse');
        head.setAttribute('d','M0 0 L10 5 L0 10 Z'); head.setAttribute('fill',color);
        marker.appendChild(head); defs.appendChild(marker);
        path.setAttribute('marker-'+endName,'url(#'+id+')');
      }
      overlay.appendChild(path);
      if (edge.label) {
        const length=path.getTotalLength();
        const p=path.getPointAtLength(length*(edge.label_at ?? 500)/1000);
        const text=svgEl('text'); text.classList.add('mesh-edge-label');
        text.setAttribute('x',p.x); text.setAttribute('y',p.y-8);
        text.setAttribute('text-anchor','middle'); text.textContent=edge.label;
        labels.appendChild(text);
      }
    });
    for (const [origin,{overlay,labels}] of layers) {
      origin.appendChild(overlay); origin.appendChild(labels);
    }
  }
  let timer;
  const redraw=()=>{clearTimeout(timer);timer=setTimeout(draw,40)};
  draw(); window.addEventListener('load',draw); window.addEventListener('resize',redraw);
  tree.addEventListener('toggle',redraw,true);
})();
