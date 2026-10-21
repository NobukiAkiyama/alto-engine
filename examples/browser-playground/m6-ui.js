const $=(s)=>document.querySelector(s);
const color=(hex,alpha=1)=>[1,3,5].map(i=>parseInt(hex.slice(i,i+2),16)/255).concat(alpha);
const hex=(c)=>'#'+c.slice(0,3).map(v=>Math.round(v*255).toString(16).padStart(2,'0')).join('');
const clone=(v)=>structuredClone(v);
export function installM6({engine,getDocument,getSelected,select,attach,run,fail}) {
  let editing=false,drag=null,selectedPath=0,lastKey='',ticket=0;
  const current=()=>{const d=getDocument(),id=getSelected();return {d,id,n:d&&id?d.view.node(id):null};};
  const style=(data)=>({data,fill:$('#vector-fill-on').checked?color($('#vector-fill').value,Number($('#vector-alpha').value)):null,stroke:$('#vector-stroke-on').checked?{color:color($('#vector-stroke').value,Number($('#vector-alpha').value)),width:Number($('#vector-width').value),cap:$('#vector-cap').value,join:$('#vector-join').value,miter_limit:4}:null,fill_rule:$('#vector-rule').value});
  function draw(data) {
    const overlay=$('#vector-handles');overlay.replaceChildren();
    if(!editing)return;
    const {n,d}=current();if(!n||n.kind.kind!=='vector')return;
    const p=data||n.kind.paths[selectedPath]?.data;if(!p)return;
    const world=[1,0,0,1,0,0];
    const chain=[];let node=n;while(node){chain.unshift(node.props.transform);node=node.parent?d.view.node(node.parent):null;}
    const mul=(a,b)=>[a[0]*b[0]+a[2]*b[1],a[1]*b[0]+a[3]*b[1],a[0]*b[2]+a[2]*b[3],a[1]*b[2]+a[3]*b[3],a[0]*b[4]+a[2]*b[5]+a[4],a[1]*b[4]+a[3]*b[5]+a[5]];
    const m=chain.reduce(mul,world);const ratio=Math.min(devicePixelRatio||1,2),z=engine.viewport.zoom/ratio,a=engine.viewport.rotation,c=Math.cos(a),s=Math.sin(a),r=engine.canvas.getBoundingClientRect(),[cx,cy]=engine.viewport.center;
    const screen=(x,y)=>{const dx=m[0]*x+m[2]*y+m[4]-cx,dy=m[1]*x+m[3]*y+m[5]-cy;return[r.width/2+z*(c*dx-s*dy),r.height/2+z*(s*dx+c*dy)];};
    overlay.setAttribute('viewBox',`0 0 ${r.width} ${r.height}`);
    for(const [i,cmd] of p.commands.entries()) {
      const points=[];for(const [x,y]of [['c1x','c1y'],['c2x','c2y'],['cx','cy'],['x','y']]) if(x in cmd)points.push({x,y,p:screen(cmd[x],cmd[y]),endpoint:x==='x'});
      if(points.length>1){const line=document.createElementNS('http://www.w3.org/2000/svg','polyline');line.setAttribute('points',points.map(q=>q.p.join(',')).join(' '));line.setAttribute('class','vector-control-line');overlay.append(line);}
      for(const point of points){const e=document.createElementNS('http://www.w3.org/2000/svg','circle');e.setAttribute('cx',point.p[0]);e.setAttribute('cy',point.p[1]);e.setAttribute('r',point.endpoint?5:4);e.setAttribute('class',point.endpoint?'vector-anchor':'vector-control');e.dataset.command=i;e.dataset.x=point.x;e.dataset.y=point.y;e.onpointerdown=(event)=>{
        event.preventDefault();event.stopPropagation();if(n.props.locked)return;
        const det=m[0]*m[3]-m[1]*m[2];if(Math.abs(det)<1e-12)return;
        drag={pointer:event.pointerId,element:e,data:clone(p),m,det,command:i,x:point.x,y:point.y,layer:n.id,path:selectedPath};e.setPointerCapture(event.pointerId);
      };e.onpointermove=(event)=>{if(!drag||drag.element!==e)return;const doc=engine.viewport.screenToDocument(event.clientX,event.clientY),dx=doc[0]-m[4],dy=doc[1]-m[5];drag.data.commands[i][point.x]=(m[3]*dx-m[2]*dy)/drag.det;drag.data.commands[i][point.y]=(-m[1]*dx+m[0]*dy)/drag.det;
        // Keep the captured handle in the DOM while dragging.
        e.setAttribute('cx',event.clientX-r.left);e.setAttribute('cy',event.clientY-r.top);
      };e.onpointerup=()=>{const g=drag;if(!g||g.element!==e)return;drag=null;run(async()=>{const d=getDocument(),node=d.view.node(g.layer);const paths=clone(node.kind.paths);paths[g.path].data=g.data;await d.vectors.set(g.layer,paths);refresh();});};e.onpointercancel=()=>{drag=null;draw();};overlay.append(e);}
    }
  }
  async function loadEditor(n) {
    const seq=++ticket;if(n?.kind.kind!=='vector'){ $('#vector-path-index').replaceChildren();return;}
    const options=n.kind.paths.map((p,i)=>{const o=document.createElement('option');o.value=i;o.textContent=`パス ${i+1}`;return o;});$('#vector-path-index').replaceChildren(...options);
    selectedPath=Math.min(selectedPath,Math.max(0,n.kind.paths.length-1));$('#vector-path-index').value=selectedPath;
    const p=n.kind.paths[selectedPath];if(!p){$('#vector-data').value='';return;}
    const svg=await engine.vectors.toSvg(p.data);if(seq!==ticket)return;$('#vector-data').value=svg;
    $('#vector-fill-on').checked=!!p.fill;$('#vector-stroke-on').checked=!!p.stroke;
    if(p.fill)$('#vector-fill').value=hex(p.fill);if(p.stroke){$('#vector-stroke').value=hex(p.stroke.color);$('#vector-width').value=p.stroke.width;$('#vector-cap').value=p.stroke.cap;$('#vector-join').value=p.stroke.join;}
    $('#vector-alpha').value=p.fill?.[3]??p.stroke?.color[3]??1;$('#vector-rule').value=p.fill_rule;
    draw();
  }
  function refresh(){const{d,n}=current();for(const id of ['vector-add','vector-rectangle','vector-ellipse','vector-curve','test-m6','sample-m6'])$('#'+id).disabled=!d;
    for(const id of ['vector-update','vector-add-path','vector-remove-path','vector-edit'])$('#'+id).disabled=n?.kind.kind!=='vector';
    for(const id of ['vector-mask-set','clip-path-set'])$('#'+id).disabled=!n;
    $('#vector-mask-remove').disabled=$('#vector-mask-load').disabled=!n?.props.vector_mask;$('#clip-path-remove').disabled=$('#clip-path-load').disabled=!n?.props.clip_path;
    $('#vector-mask-enabled').disabled=!n?.props.vector_mask;$('#vector-mask-enabled').checked=!!n?.props.vector_mask?.enabled;
    $('#clip-path-enabled').disabled=!n?.props.clip_path;$('#clip-path-enabled').checked=!!n?.props.clip_path?.enabled;
    if(n?.props.vector_mask)$('#vector-mask-feather').value=n.props.vector_mask.feather;
    const key=(n?.id||'')+JSON.stringify(n?.kind);if(key!==lastKey){lastKey=key;void loadEditor(n).catch(fail);}else draw();}
  async function add(svg){const{d,n}=current();const data=await engine.vectors.parseSvg(svg||$('#vector-data').value);let id;
    await d.transact('ベクター追加',tx=>{id=tx.addVector([style(data)],{parent:n?.kind.kind==='group'?n.id:n?.parent||d.view.root.id});});select(id);refresh();}
  $('#vector-add').onclick=()=>run(()=>add());
  for(const[id,svg]of [['vector-rectangle','M 100 100 H 420 V 320 H 100 Z'],['vector-ellipse','M 440 240 A 180 130 0 1 0 80 240 A 180 130 0 1 0 440 240 Z'],['vector-curve','M 90 300 C 150 20 390 20 450 300 Q 260 420 90 300 Z']])$('#'+id).onclick=()=>run(()=>add(svg));
  $('#vector-update').onclick=()=>run(async()=>{const{d,id,n}=current();const data=await engine.vectors.parseSvg($('#vector-data').value),paths=clone(n.kind.paths);paths[selectedPath]=style(data);await d.vectors.set(id,paths);});
  $('#vector-add-path').onclick=()=>run(async()=>{const{d,id,n}=current();const data=await engine.vectors.parseSvg($('#vector-data').value);selectedPath=n.kind.paths.length;await d.vectors.set(id,[...n.kind.paths,style(data)]);});
  $('#vector-remove-path').onclick=()=>run(async()=>{const{d,id,n}=current();await d.vectors.set(id,n.kind.paths.filter((p,i)=>i!==selectedPath));});
  $('#vector-path-index').onchange=()=>{selectedPath=Number($('#vector-path-index').value);void loadEditor(current().n).catch(fail);};
  $('#vector-edit').onclick=()=>{editing=!editing;$('#vector-edit').classList.toggle('active-tool',editing);$('#vector-edit').textContent=editing?'点の編集を終了':'パスの点を編集';draw();};
  for(const b of document.querySelectorAll('[data-tool],[data-selection]'))b.addEventListener('click',()=>{editing=false;$('#vector-edit').classList.remove('active-tool');$('#vector-edit').textContent='パスの点を編集';draw();});
  for(const [id,type]of [['vector-mask-set','mask'],['clip-path-set','clip']])$('#'+id).onclick=()=>run(async()=>{const{d,id:layer}=current();const path=await engine.vectors.parseSvg($('#vector-data').value),value={path,fill_rule:$('#vector-rule').value,enabled:true,transform:[1,0,0,1,0,0]};if(type==='mask'){value.transform=current().n.props.vector_mask?.transform||value.transform;value.feather=Number($('#vector-mask-feather').value);await d.vectors.setMask(layer,value);}else {value.transform=current().n.props.clip_path?.transform||value.transform;await d.vectors.setClipPath(layer,value);}});
  for(const [id,type]of [['vector-mask-load','vector_mask'],['clip-path-load','clip_path']])$('#'+id).onclick=()=>run(async()=>{const p=current().n.props[type];if(!p)return;$('#vector-data').value=await engine.vectors.toSvg(p.path);$('#vector-rule').value=p.fill_rule;if(type==='vector_mask')$('#vector-mask-feather').value=p.feather;});
  $('#vector-mask-remove').onclick=()=>run(()=>{const{d,id}=current();return d.vectors.setMask(id,null);});$('#clip-path-remove').onclick=()=>run(()=>{const{d,id}=current();return d.vectors.setClipPath(id,null);});
  for(const[id,type]of [['vector-mask-enabled','vector_mask'],['clip-path-enabled','clip_path'],['vector-mask-feather','vector_mask']])$('#'+id).onchange=()=>run(()=>{const{d,id:layer,n}=current();const value=clone(n.props[type]);if(!value)return;value.enabled=$('#'+(type==='vector_mask'?'vector-mask-enabled':'clip-path-enabled')).checked;if(type==='vector_mask')value.feather=Number($('#vector-mask-feather').value);return type==='vector_mask'?d.vectors.setMask(layer,value):d.vectors.setClipPath(layer,value);});
  $('#view-zoom').onchange=()=>engine.viewport.set({zoom:Number($('#view-zoom').value)*Math.min(devicePixelRatio||1,2)/100});
  engine.on('viewport',()=>{ $('#view-zoom').value=Math.round(engine.viewport.zoom/Math.min(devicePixelRatio||1,2)*100);draw();});engine.on('canvasReplaced',()=>{drag=null;draw();});
  $('#sample-m6').onclick=()=>run(async()=>attach(await engine.openDocument(await(await fetch('../../verification/sample-m6.alto')).blob())));
  $('#test-m6').onclick=()=>run(async()=>{const b=$('#test-m6');b.disabled=true;$('#error').textContent='';$('#test-results').textContent='M6を検証中…';try{const{runM6Tests}=await import('../../tests/e2e/m6-tests.js');await runM6Tests(engine,line=>$('#test-results').textContent+='\n'+line);attach(engine.document);}finally{b.disabled=false;}});
  refresh();$('#view-zoom').value=Math.round(engine.viewport.zoom/Math.min(devicePixelRatio||1,2)*100);return{refresh};
}
