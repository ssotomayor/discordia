const fs=require('node:fs'),path=require('node:path'),crypto=require('node:crypto');
const root=path.resolve(__dirname,'..'), directory=path.join(root,'target/dm-server-images');
const raw=JSON.parse(fs.readFileSync(path.join(root,'docs/DM_SERVER_IMAGE_RESULTS.json')));
const preparation=[1,2,3,4,5].flatMap(n=>JSON.parse(fs.readFileSync(path.join(directory,`preparation-${n}.json`))));
const stats=values=>{const a=values.sort((x,y)=>x-y),n=a.length,mean=a.reduce((s,x)=>s+x,0)/n;return {n,median:a[Math.floor(n/2)],min:a[0],max:a[n-1],sampleSD:Math.sqrt(a.reduce((s,x)=>s+(x-mean)**2,0)/(n-1))};};
const metrics={rustMiB:s=>s.rustPrivateMiB,rendererMiB:s=>s.rendererPrivateMiB,gpuPrivateMiB:s=>s.gpuPrivateMiB,totalPrivateMiB:s=>s.summary.TotalPrivateCommitMb,workingSetMiB:s=>s.summary.TotalResidentWorkingSetMb,idleCpuPercent:s=>s.summary.TotalCpuPercent,loadMs:s=>s.loadMs,cpuCoreSeconds:s=>s.loadingCpu.total,encodedMiB:s=>s.cache.encodedBytes/1048576,corpusMiB:s=>s.cache.corpusBytes/1048576,cacheEntries:s=>s.cache.entries,imageCount:s=>s.imageCount};
const output={scope:'Real production image preparation functions and Windows/WebView2 component fixture; excludes network, cryptography and whole-client state. GPU private memory is not VRAM.',variants:{},processing:{},preparation};
for(const variant of [...new Set(raw.samples.map(s=>s.variant))]){
  const a=raw.samples.filter(s=>s.variant===variant);
  if(a.length!==5||new Set(a.map(s=>s.run)).size!==5)throw Error(`Five independent runs required: ${variant}`);
  output.variants[variant]=Object.fromEntries(Object.entries(metrics).map(([k,get])=>[k,stats(a.map(get))]));
}
for(const name of [...new Set(preparation.map(s=>s.case))]){
  const rows=preparation.filter(s=>s.case===name),times=['serverSendMs','dmSendMs','dmColdImageWorkMs'];
  if(name==='jpeg-large')times.push('server2MiBSendMs');
  output.processing[name]=Object.fromEntries(times.map(metric=>[metric,stats([1,2,3,4,5].map(n=>{const run=rows.filter(r=>r.run===n);return run.reduce((sum,r)=>sum+r[metric],0)/run.length;}))]));
}
output.sourceHashes=Object.fromEntries(['client/src/chat_image.rs','client/src/image_assets.rs','scripts/measure-image-memory.cjs'].map(f=>[f,crypto.createHash('sha256').update(fs.readFileSync(path.join(root,f))).digest('hex')]));
fs.writeFileSync(path.join(root,'docs/DM_SERVER_IMAGE_STATISTICS.json'),JSON.stringify(output,null,2)+'\n');
const number=n=>n.toFixed(2),format=s=>`${number(s.median)} [${number(s.min)}–${number(s.max)}]; SD ${number(s.sampleSD)}`;
const lines=['# DM y comunidades — mediciones','',output.scope,'','Cinco procesos por variante, ocho imágenes visibles y perfiles nuevos. Medianas; MiB = 1048576 bytes. La carga parte de los recursos preparados en RAM y termina tras complete y dos frames; no fuerza img.decode().','', '| Variante | Rust MiB | Renderer MiB | GPU Private MiB | Total MiB | Working Set MiB |','|---|---:|---:|---:|---:|---:|'];
for(const [name,s] of Object.entries(output.variants))lines.push(`| ${name} | ${['rustMiB','rendererMiB','gpuPrivateMiB','totalPrivateMiB','workingSetMiB'].map(k=>number(s[k].median)).join(' | ')} |`);
lines.push('','| Variante | Carga ms [mín–máx]; SD | CPU de carga, segundos de núcleo [mín–máx]; SD | CPU posterior % [mín–máx]; SD |','|---|---:|---:|---:|');
for(const [name,s] of Object.entries(output.variants))lines.push(`| ${name} | ${format(s.loadMs)} | ${format(s.cpuCoreSeconds)} | ${format(s.idleCpuPercent)} |`);
lines.push('','Preparación: media por imagen dentro de cada lote, mediana de cinco procesos. Tiempo de pared; no equivale a CPU ni al tiempo total de recepción concurrente. La lectura para comunidad fue primero, luego DM: puede haber calentamiento; no se atribuye su diferencia al recorrido. No es el envío completo. DM frío incluye validación/decodificación y generación de miniatura; excluye descarga, hashes, AES y disco cifrado.','', '| Caso | Leer/validar para adjuntar comunidad ms | Leer/validar para adjuntar DM ms | Trabajo de imagen al recibir DM frío ms |','|---|---:|---:|---:|');
for(const [name,s] of Object.entries(output.processing))lines.push(`| ${name} | ${format(s.serverSendMs)} | ${format(s.dmSendMs)} | ${format(s.dmColdImageWorkMs)} |`);
lines.push('','| Caso, imagen 0 | Fuente | Comunidad por defecto | Miniatura DM |','|---|---|---|---|');
const asset=a=>`${a.width}×${a.height}; ${a.mime}; ${a.bytes} bytes; ${a.frames} frame(s); ${a.orientation}`;
for(const name of Object.keys(output.processing)){const r=preparation.find(s=>s.case===name&&s.index===0);lines.push(`| ${name} | ${asset(r.input)} | ${asset(r.server)} | ${asset(r.dmThumbnail)} |`);}
lines.push('','Todos los recursos de comunidades por defecto y los originales enviados por DM conservaron sus bytes en este corpus: '+preparation.every(r=>r.serverUnchanged&&r.dmOriginalUnchanged)+'. Política reducida de 2 MiB: caso separado, no el valor por defecto.','');
fs.writeFileSync(path.join(root,'docs/DM_SERVER_IMAGE_MEASUREMENTS.md'),lines.join('\n'));
console.log('Wrote complete DM/server statistics and tables');
