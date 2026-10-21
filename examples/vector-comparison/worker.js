import init,{compareVectorBackends} from './pkg/alto_wasm.js';
self.onmessage=async()=>{try{await init();postMessage(JSON.parse(await compareVectorBackends()));}catch(error){postMessage({error:String(error.stack||error)});}};
