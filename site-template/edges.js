/* Browser-measured canvas edges. Author waypoints are relative to the source port. */
(() => {
  const data = document.getElementById('mesh-edge-data');
  if (!data) return;
  const edges = JSON.parse(data.textContent);
  const tree = document.querySelector('.tree');
  if (!tree) return;
  const allTree = tree.classList.contains('all-svg-edges');
  const ns = 'http://www.w3.org/2000/svg';
  const svgEl = name => document.createElementNS(ns, name);
  const box = el => {
    const a = el.getBoundingClientRect(), b = tree.getBoundingClientRect();
    return { left: a.left - b.left, top: a.top - b.top, right: a.right - b.left,
      bottom: a.bottom - b.top, cx: (a.left + a.right) / 2 - b.left,
      cy: (a.top + a.bottom) / 2 - b.top };
  };
  const port = (b, side) => ({
    left: {x:b.left,y:b.cy}, right: {x:b.right,y:b.cy},
    top: {x:b.cx,y:b.top}, bottom: {x:b.cx,y:b.bottom}
  })[side];
  const choose = (a,b) => Math.abs(b.cx-a.cx) >= Math.abs(b.cy-a.cy)
    ? (b.cx >= a.cx ? ['right','left'] : ['left','right'])
    : (b.cy >= a.cy ? ['bottom','top'] : ['top','bottom']);
  let overlay, labels;
  function draw() {
    if (overlay) overlay.remove();
    if (labels) labels.remove();
    tree.querySelectorAll('.edge-overridden').forEach(el => el.classList.remove('edge-overridden'));
    overlay = svgEl('svg');
    overlay.classList.add('mesh-edge-overlay');
    overlay.setAttribute('width', Math.max(tree.scrollWidth, tree.clientWidth));
    overlay.setAttribute('height', Math.max(tree.scrollHeight, tree.clientHeight));
    labels = svgEl('svg'); labels.classList.add('mesh-edge-label-overlay');
    labels.setAttribute('width', overlay.getAttribute('width'));
    labels.setAttribute('height', overlay.getAttribute('height'));
    const defs = svgEl('defs'); overlay.appendChild(defs);
    const defaultColor = getComputedStyle(document.documentElement).getPropertyValue('--accent-default').trim() || '#9b4437';
    let drawn = 0;
    edges.forEach((edge, i) => {
      if (edge.kind === 'tree' && !allTree && !edge.via.length && !edge.source_side && !edge.target_side && !edge.label) return;
      const from = document.getElementById('node-' + edge.from);
      const to = document.getElementById('node-' + edge.to);
      if (!from || !to || !from.getClientRects().length || !to.getClientRects().length) return;
      if (edge.kind === 'tree' && !allTree) to.closest('.node-row')?.classList.add('edge-overridden');
      const a=box(from), b=box(to), sides=choose(a,b);
      const start=port(a,edge.source_side || sides[0]), end=port(b,edge.target_side || sides[1]);
      const points=[start,...edge.via.map(p=>({x:start.x+p.x,y:start.y+p.y})),end];
      const path=svgEl('path');
      let route;
      if (edge.via.length || edge.source_side || edge.target_side) {
        const horizontal = ['left','right'].includes(edge.source_side || sides[0]);
        route='M'+start.x+','+start.y;
        for (let j=1;j<points.length;j++) {
          const prev=points[j-1], next=points[j];
          if (prev.x!==next.x && prev.y!==next.y)
            route += horizontal ? ' L'+next.x+','+prev.y : ' L'+prev.x+','+next.y;
          route += ' L'+next.x+','+next.y;
        }
      } else if (edge.kind==='extra') {
        const bend=Math.max(Math.abs(end.x-start.x)*.5,30);
        route=`M${start.x},${start.y} C${start.x+bend},${start.y} ${end.x-bend},${end.y} ${end.x},${end.y}`;
      } else {
        route=`M${start.x},${start.y} L${end.x},${end.y}`;
      }
      path.setAttribute('d',route);
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
      drawn++;
      if (edge.label) {
        const length=path.getTotalLength();
        const p=path.getPointAtLength(length*(edge.label_at ?? 500)/1000);
        const text=svgEl('text'); text.classList.add('mesh-edge-label');
        text.setAttribute('x',p.x); text.setAttribute('y',p.y-8);
        text.setAttribute('text-anchor','middle'); text.textContent=edge.label;
        labels.appendChild(text);
      }
    });
    if (drawn) { tree.appendChild(overlay); tree.appendChild(labels); }
  }
  let timer;
  const redraw=()=>{clearTimeout(timer);timer=setTimeout(draw,40)};
  draw(); window.addEventListener('load',draw); window.addEventListener('resize',redraw);
  tree.addEventListener('toggle',redraw,true);
})();
