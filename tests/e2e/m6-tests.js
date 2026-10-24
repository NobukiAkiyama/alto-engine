const assert=(v,m)=>{if(!v)throw Error(m);};
const same=(a,b)=>a.length===b.length&&a.every((v,i)=>v===b[i]);
const near=(a,b,t=2)=>a.every((v,i)=>Math.abs(v-b[i])<=t);
const pixel=(t,x,y)=>Array.from(t.slice((y*256+x)*4,(y*256+x)*4+4));
async function at(e,x,y){return pixel(await e.unstable.readTile(Math.floor(x/256),Math.floor(y/256)),x%256,y%256);}
async function styled(e,svg,options={}){return {data:await e.vectors.parseSvg(svg),fill:[1,0,0,1],stroke:null,fill_rule:'non_zero',...options};}
const frame=async e=>{await e.request('validateFrame');await e.unstable.settle();};
export async function runM6Tests(engine,log){
 const results=[];let doc,layer;const metrics={zoom:[]};
 const test=async(name,fn)=>{const start=performance.now();await fn();results.push({name,pass:true,ms:performance.now()-start});log('PASS '+name);};
 try{
 await test('SVG paths / cubic and quadratic / even-odd and non-zero holes',async()=>{
   doc=await engine.createDocument({width:512,height:512});
   const p=await styled(engine,'M32 32 H480 V480 H32 Z M160 160 H352 V352 H160 Z',{fill_rule:'even_odd'});
   const svg=await engine.vectors.toSvg(p.data);assert(JSON.stringify(await engine.vectors.parseSvg(svg))===JSON.stringify(p.data),'path roundtrip');
   await doc.transact('vector',tx=>layer=tx.addVector([p]));await frame(engine);
   assert(near(await at(engine,64,64),[255,0,0,255]),'fill missing');assert((await at(engine,256,256))[3]===0,'even-odd hole');
   await doc.vectors.set(layer,[{...p,fill_rule:'non_zero'}]);assert((await at(engine,256,256))[3]===255,'non-zero winding');await doc.undo();assert((await at(engine,256,256))[3]===0,'fill-rule Undo');
   const q=await styled(engine,'M40 300 Q150 20 260 300 C300 400 440 20 470 300 Z');await doc.vectors.set(layer,[q]);await frame(engine);assert((await at(engine,150,200))[3]>0,'curves missing');
 });
 await test('stroke caps / joins / translucent self-overlap uses coverage union',async()=>{
   doc=await engine.createDocument({width:512,height:512});const stroke={color:[0,0.5,1,0.5],width:32,cap:'butt',join:'round',miter_limit:4};
   const p=await styled(engine,'M100 100 H300',{fill:null,stroke});await doc.transact('stroke',tx=>layer=tx.addVector([p]));await frame(engine);
   assert((await at(engine,90,100))[3]===0,'butt cap extended');assert(near(await at(engine,200,100),[0,64,128,128]),'stroke premultiply');
   await doc.vectors.set(layer,[{...p,stroke:{...stroke,cap:'round'}}]);assert((await at(engine,90,100))[3]>=127,'round cap missing');
   await doc.vectors.set(layer,[await styled(engine,'M80 80 L300 300 M80 300 L300 80',{fill:null,stroke})]);assert(Math.abs((await at(engine,190,190))[3]-128)<=1,'self-overlap accumulated alpha');
   for(const join of ['miter','bevel','round']){await doc.vectors.set(layer,[await styled(engine,'M80 240 L200 80 L320 240',{fill:null,stroke:{...stroke,join}})]);await frame(engine);assert((await at(engine,200,90))[3]>0,'join '+join);}
 });
 await test('editing control points / styles / transforms is reversible',async()=>{
   doc=await engine.createDocument({width:512,height:512});const p=await styled(engine,'M60 250 Q180 20 300 250 L300 400 H60 Z');await doc.transact('path',tx=>layer=tx.addVector([p]));
   const before=await engine.unstable.readTile(0,0),paths=JSON.stringify(doc.view.node(layer).kind.paths),edited=structuredClone(p);edited.data.commands[1].cy=200;edited.fill=[0,1,0,1];
   await doc.vectors.set(layer,[edited]);const after=await engine.unstable.readTile(0,0);assert(!same(before,after),'edit did not change GPU pixels');await doc.undo();assert(same(before,await engine.unstable.readTile(0,0)),'path Undo pixels');assert(paths===JSON.stringify(doc.view.node(layer).kind.paths),'path Undo data');await doc.redo();assert(same(after,await engine.unstable.readTile(0,0)),'path Redo pixels');
   await doc.transact('shear',tx=>tx.setTransform(layer,[1,.1,.25,.8,20,10]));await frame(engine);assert((await at(engine,220,270))[1]>0,'transformed vector');await doc.undo();assert(same(after,await engine.unstable.readTile(0,0)),'transform Undo');
 });
 await test('zoom levels -6 through 12 retain analytic curve contours across tile seams',async()=>{
   doc=await engine.createDocument({width:65536,height:65536});const p=await styled(engine,'M128 192 Q256 64 384 192 L384 400 H128 Z',{fill:[1,1,1,1]});await doc.transact('analytic curve',tx=>layer=tx.addVector([p]));
   for(const level of [-6,-5,-4,-3,-2,-1,0,1,2,3,4,5,6,8,10,12]){
     // Scale the same exact quadratic for minification; keep it within the document at L10/12.
     const transform=Math.min(128,2**Math.max(0,level)),scale=2**level;
     await doc.transact('zoom case',tx=>tx.setTransform(layer,[transform,0,0,transform,0,0]));
     const px=192*transform,py=144*transform,tx0=Math.floor(px/(256*scale)),ty0=Math.floor(py/(256*scale));let checked=0,partial=0;
     for(const ty of new Set([Math.max(0,ty0-1),ty0]))for(const tx of new Set([Math.max(0,tx0-1),tx0])){
       const t=await engine.unstable.readTileLevel(level,tx,ty);
       for(let y=0;y<256;y++)for(let x=0;x<256;x++){
         const wx=(tx*256+x+.5)*scale/transform,wy=(ty*256+y+.5)*scale/transform;
         if(wx<132||wx>380||wy<100||wy>380)continue;
         const curve=128+(wx-256)**2/256,slope=(wx-256)/128,distance=(wy-curve)/Math.sqrt(1+slope*slope)*transform/scale,a=t[(y*256+x)*4+3];
         if(distance>1.2&&wy<398){assert(a===255,`interior seam L${level} at ${wx},${wy}: ${a}`);checked++;}
         if(distance< -1.2){assert(a===0,`exterior L${level}: ${a}`);checked++;}
         if(Math.abs(distance)<1.2&&a>0&&a<255)partial++;
       }
     }
     assert(checked>0,'no zoom samples L'+level);if(level<=8)assert(partial>0,'MSAA contour missing L'+level);metrics.zoom.push({level,documentPixelsPerOutputPixel:scale,checked,partial});
   }
   const stats=await engine.unstable.vectorStats();assert(stats.entries>1,'level-band mesh cache absent');metrics.meshCache=stats;
 });
 await test('vector masks / feather / raster mask intersection / canonical raster unchanged',async()=>{
   doc=await engine.createDocument({width:512,height:512});const canvas=new OffscreenCanvas(512,512),c=canvas.getContext('2d');c.fillStyle='blue';c.fillRect(0,0,512,512);const asset=await engine.assets.importImage(await canvas.convertToBlob());await doc.transact('raster',tx=>layer=tx.addRasterFromAsset(asset));const original=JSON.stringify(await doc.nodeTiles(layer));
   const path=await engine.vectors.parseSvg('M128 64 H384 V448 H128 Z');await doc.vectors.setMask(layer,{path,feather:0});assert((await at(engine,200,200))[3]===255,'vector mask interior');assert((await at(engine,64,200))[3]===0,'vector mask exterior');
   await doc.vectors.setMask(layer,{path,feather:12});const edge=await at(engine,128,250);assert(edge[3]>80&&edge[3]<190,'feather missing');assert((await at(engine,120,250))[3]>0,'feather halo missing');
   await doc.selection.apply({kind:'rectangle',x:0,y:0,width:256,height:512});await doc.masks.fromSelection(layer);await doc.selection.clear();assert((await at(engine,300,200))[3]===0,'raster mask intersection');assert((await at(engine,200,200))[3]===255,'mask intersection inside');
   assert(original===JSON.stringify(await doc.nodeTiles(layer)),'vector mask modified source');await doc.vectors.setMask(layer,{path,enabled:false});assert((await at(engine,64,200))[3]===255,'mask disable');await doc.undo();assert((await at(engine,64,200))[3]===0,'mask Undo');
 });
 await test('clipping paths on groups / adjustments / vector-mask intersection',async()=>{
   doc=await engine.createDocument({width:512,height:512});const p=await styled(engine,'M0 0 H512 V512 H0 Z',{fill:[1,0,0,1]});let group,adjust;
   await doc.transact('group',tx=>{group=tx.addGroup({isolated:false});layer=tx.addVector([p],{parent:group});adjust=tx.addAdjustment({type:'color_adjust',brightness:.4},{parent:group});});
   const half=await engine.vectors.parseSvg('M0 0 H256 V512 H0 Z'),strip=await engine.vectors.parseSvg('M0 120 H512 V380 H0 Z');
   await doc.vectors.setClipPath(group,{path:half});assert((await at(engine,300,200))[3]===0,'group clipping escaped');assert((await at(engine,100,200))[1]>80,'adjustment not applied');
   await doc.vectors.setMask(adjust,{path:strip});assert((await at(engine,100,64))[1]===0,'adjustment vector mask ignored');assert((await at(engine,100,200))[1]>80,'masked adjustment missing');
   await doc.vectors.setMask(group,{path:strip});assert((await at(engine,100,64))[3]===0,'group mask intersection');assert((await at(engine,100,200))[3]===255,'group intersection interior');
   await doc.vectors.setClipPath(group,{path:half,enabled:false});assert((await at(engine,300,200))[3]===255,'clip enable flag');await frame(engine);
 });
 await test('vector blur / layer clipping / mask moves with parent',async()=>{
   doc=await engine.createDocument({width:768,height:512});const p=await styled(engine,'M220 220 H290 V290 H220 Z',{fill:[1,0,0,.5]});let group,top;
   await doc.transact('vector clipping',tx=>{group=tx.addGroup({isolated:true});layer=tx.addVector([p],{parent:group});top=tx.addVector([{...p,fill:[0,0,1,1]}],{parent:group});tx.setProps(top,{clip_to_below:true});});
   assert(near(await at(engine,250,250),[0,0,128,128]),'vector clipping alpha');const source=JSON.stringify(doc.view.node(layer).kind.paths);await doc.effects.set(group,[{type:'gaussian_blur',radius:12}],{useSelection:false});assert((await at(engine,216,250))[3]>0,'vector blur halo');
   const clip=await engine.vectors.parseSvg('M0 0 H256 V512 H0 Z');await doc.vectors.setClipPath(group,{path:clip});assert((await at(engine,270,250))[3]===0,'post-effect clip');await doc.transact('parent movement',tx=>tx.setTransform(group,[1,0,0,1,60,0]));assert((await at(engine,310,250))[3]>0,'clip did not follow parent');assert((await at(engine,330,250))[3]===0,'moved clip exterior');assert(source===JSON.stringify(doc.view.node(layer).kind.paths),'blur modified vector data');
 });
 await test('64x zoom keeps selected blur, backdrop blur and feather within ROI budgets',async()=>{
   doc=await engine.createDocument({width:512,height:512});const p=await styled(engine,'M220 220 H290 V290 H220 Z',{fill:[1,1,1,1]});await doc.transact('zoom blur',tx=>layer=tx.addVector([p]));
   await doc.selection.apply({kind:'rectangle',x:0,y:0,width:256,height:512});await doc.effects.set(layer,[{type:'gaussian_blur',radius:12}]);await doc.selection.clear();
   const fine=await engine.unstable.readTileLevel(-6,72,62);assert(pixel(fine,127,128)[3]===255,'unselected sharp edge lost at 64x');assert(pixel(fine,128,128)[3]===0,'unselected sharp exterior lost');
   let adjust;await doc.transact('backdrop blur',tx=>adjust=tx.addAdjustment({type:'color_adjust',brightness:0}));await doc.effects.set(adjust,[{type:'gaussian_blur',radius:12}],{useSelection:false});
   const left=await engine.unstable.readTileLevel(-6,54,62),right=await engine.unstable.readTileLevel(-6,55,62);assert(Math.abs(pixel(left,255,128)[3]-pixel(right,0,128)[3])<=2,'blur seam at high zoom');assert(pixel(right,0,128)[3]>30&&pixel(right,0,128)[3]<230,'backdrop blur missing at high zoom');
   await doc.vectors.setMask(layer,{path:p.data,feather:64});await engine.unstable.readTileLevel(-6,55,62);await frame(engine);
 });
 await test('mesh cache reuse / invalid vector edits reject atomically',async()=>{
   doc=await engine.createDocument({width:512,height:512});const p=await styled(engine,'M100 100 Q256 0 400 100 V400 H100 Z');await doc.transact('path',tx=>layer=tx.addVector([p]));await engine.unstable.readTile(0,0);const a=await engine.unstable.vectorStats();await doc.transact('opacity',tx=>tx.setProps(layer,{opacity:.8}));await engine.unstable.readTile(0,0);const b=await engine.unstable.vectorStats();assert(b.hits>a.hits,'mesh was not reused');assert(b.misses===a.misses,'opacity retessellated');
   const snapshot=(await doc.snapshotHash()),revision=doc.view.revision;
   for(const bad of [[],[{op:'line',x:2,y:3}],[{op:'move',x:1e20,y:0}],Array(4097).fill({op:'move',x:0,y:0})]){
     if(!bad.length)continue;let failed=false;try{await doc.vectors.set(layer,[{...p,data:{commands:bad}}]);}catch{failed=true;}assert(failed,'invalid path accepted');
   }
   let failed=false;try{await doc.vectors.set(layer,[{...p,stroke:{color:[1,0,0,1],width:-1}}]);}catch{failed=true;}assert(failed,'negative stroke');
   failed=false;try{await doc.vectors.setMask(layer,{path:p.data,feather:100});}catch{failed=true;}assert(failed,'invalid feather');assert(snapshot===(await doc.snapshotHash())&&revision===doc.view.revision,'failed edits changed document');await frame(engine);
 });
 await test('M6 ZIP / GPU loss / journal recovery retain paths, masks, clips and pixels',async()=>{
   const p=doc.view.node(layer).kind.paths[0];await doc.vectors.setMask(layer,{path:p.data,feather:4});await doc.vectors.setClipPath(layer,{path:await engine.vectors.parseSvg('M0 0 H350 V512 H0 Z')});
   const hash=(await doc.snapshotHash()),before=await engine.unstable.readTile(0,0),bytes=await engine.unstable.saveBytes();doc=await engine.openDocument(new Blob([bytes]));assert(hash===(await doc.snapshotHash()),'ZIP snapshot hash');assert(same(before,await engine.unstable.readTile(0,0)),'ZIP pixels');
   await engine.unstable.destroyGpu();await new Promise((resolve,reject)=>{const off=engine.on('gpuRestored',()=>{off();resolve();});setTimeout(()=>reject(Error('GPU recovery timeout')),15000);});assert(same(before,await engine.unstable.readTile(0,0)),'GPU rebuild');
   await doc.transact('journal vector',tx=>tx.setVector(layer,[{...p,fill:[0,1,0,1]}]));const after=await engine.unstable.readTile(0,0),savedHash=(await doc.snapshotHash());await doc.durable();await engine.unstable.crash();doc=engine.document;assert(savedHash===(await doc.snapshotHash()),'journal snapshot');assert(same(after,await engine.unstable.readTile(0,0)),'journal pixels');await frame(engine);
 });
 }catch(error){results.push({name:String(error.stack||error),pass:false});log('FAIL '+error);throw error;}
 finally{const report={date:new Date().toISOString(),milestone:'M6',userAgent:navigator.userAgent,crossOriginIsolated,viewport:{width:innerWidth,height:innerHeight,dpr:devicePixelRatio},results,metrics};document.querySelector('#test-results').dataset.report=JSON.stringify(report);}
 log('M6 COMPLETE');return results;
}
