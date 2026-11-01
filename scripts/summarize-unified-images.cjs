const fs=require('node:fs'),path=require('node:path'),crypto=require('node:crypto');
const root=path.resolve(__dirname,'..'),folder=path.join(root,'target/unified-images');
const raw=JSON.parse(fs.readFileSync(path.join(root,'docs/UNIFIED_IMAGE_RESULTS.json')));
const stats=values=>{const a=values.toSorted((x,y)=>x-y),mean=a.reduce((s,v)=>s+v,0)/a.length;return {n:a.length,median:a[Math.floor(a.length/2)],min:a[0],max:a.at(-1),sampleSD:Math.sqrt(a.reduce((s,v)=>s+(v-mean)**2,0)/(a.length-1))};};
const metrics={rustMiB:s=>s.rustPrivateMiB,rendererMiB:s=>s.rendererPrivateMiB,gpuPrivateMiB:s=>s.gpuPrivateMiB,totalPrivateMiB:s=>s.summary.TotalPrivateCommitMb,workingSetMiB:s=>s.summary.TotalResidentWorkingSetMb,idleCpuPercent:s=>s.summary.TotalCpuPercent,loadMs:s=>s.loadMs,cpuCoreSeconds:s=>s.loadingCpu.total,encodedMiB:s=>s.cache.encodedBytes/1048576,corpusMiB:s=>s.cache.corpusBytes/1048576,cacheEntries:s=>s.cache.entries,imageCount:s=>s.imageCount};
if(raw.samples.length!==45)throw Error('45 independent samples required');
const preparation=[1,2,3,4,5].flatMap(n=>JSON.parse(fs.readFileSync(path.join(folder,'fixtures',`preparation-${n}.json`))));
const result={scope:raw.scope,variants:{},processing:{},server:{},preparation};
for(const variant of Object.keys(raw.binaries)){
 const samples=raw.samples.filter(s=>s.variant===variant);
 if(samples.length!==5||new Set(samples.map(s=>s.run)).size!==5||samples.some(s=>s.environment.dpr!==1||s.environment.visibility!=='visible'||!s.environment.focused||s.imageCount!==8))throw Error('Non-comparable samples: '+variant);
 result.variants[variant]=Object.fromEntries(Object.entries(metrics).map(([name,get])=>[name,stats(samples.map(get))]));
}
for(const name of new Set(preparation.map(r=>r.case))){
 const rows=preparation.filter(r=>r.case===name);
 result.processing[name]=Object.fromEntries(['serverPreviewMs','dmColdImageWorkMs'].map(metric=>[metric,stats([1,2,3,4,5].map(n=>{const run=rows.filter(r=>r.run===n);return run.reduce((s,r)=>s+r[metric],0)/run.length;}))]));
}
const timingFiles=[1,2,3,4,5].map(n=>path.join(folder,`server-timing-${n}.json`));
if(timingFiles.every(f=>fs.existsSync(f))){const samples=timingFiles.map(f=>JSON.parse(fs.readFileSync(f)));result.server={samples,coldMs:stats(samples.map(s=>s.coldMs)),cachedMs:stats(samples.map(s=>s.cachedMs))};}
result.sourceHashes=Object.fromEntries(['image-policy/src/lib.rs','client/src/image_assets.rs','client/src/features/image_attachment.rs','client/src/features/image_viewer.rs','client/assets/image-viewer.js','client/src/net.rs','server/src/media.rs','server/src/gateway/connection.rs','tools/image-memory-probe/main.rs','scripts/measure-image-memory.cjs'].map(f=>[f,crypto.createHash('sha256').update(fs.readFileSync(path.join(root,f))).digest('hex')]));
fs.writeFileSync(path.join(root,'docs/UNIFIED_IMAGE_STATISTICS.json'),JSON.stringify(result,null,2)+'\n');
const n=v=>v.toFixed(2),range=s=>`${n(s.median)} [${n(s.min)}–${n(s.max)}]; SD ${n(s.sampleSD)}`;
const lines=['# Política común — mediciones','',raw.scope,'','Windows/WebView2, cinco procesos por variante; ocho imágenes, 1440×900, DPR 1, ventana visible y enfocada, perfil nuevo. Variantes intercaladas; orden invertido en rondas pares. Sin compilaciones paralelas. Se conservó Discordia PID 10772 de otra sesión.','', 'Carga desde recursos preparados hasta complete y dos frames; sin forzar img.decode(). Excluye red, cifrado, estado del cliente completo y generación fría en servidor. GPU Private Commit no es VRAM. Working Set es la suma por proceso.','', '| Variante | Rust MiB | Renderer MiB | GPU Private MiB | Total MiB | Working Set MiB |','|---|---:|---:|---:|---:|---:|'];
for(const [name,s]of Object.entries(result.variants))lines.push(`| ${name} | ${['rustMiB','rendererMiB','gpuPrivateMiB','totalPrivateMiB','workingSetMiB'].map(k=>n(s[k].median)).join(' | ')} |`);
lines.push('','| Variante | Total MiB [mín–máx]; SD | Carga ms [mín–máx]; SD | CPU de carga, segundos de núcleo [mín–máx]; SD | CPU posterior % [mín–máx]; SD |','|---|---:|---:|---:|---:|');
for(const [name,s]of Object.entries(result.variants))lines.push(`| ${name} | ${['totalPrivateMiB','loadMs','cpuCoreSeconds','idleCpuPercent'].map(k=>range(s[k])).join(' | ')} |`);
lines.push('','| Caso | Generación de miniatura, Release ms/imagen | Validación y miniatura DM frío, Release ms/imagen |','|---|---:|---:|');
for(const [name,s]of Object.entries(result.processing))lines.push(`| ${name} | ${range(s.serverPreviewMs)} | ${range(s.dmColdImageWorkMs)} |`);
lines.push('','Preparación: media por imagen dentro de cada lote y mediana de cinco procesos. El orden fijo de preparación puede calentar el codec; sus diferencias no representan un A/B de servidor frente a DM. Excluye red, cifrado y caché del servidor.','');
if(result.server.coldMs)lines.push('MediaStore::preview en ejecutable de test Release: cinco procesos; mismo JPEG 4000×3000. Excluye subida, red, cola del semáforo y socket.','', '| Estado | ms [mín–máx]; SD |','|---|---:|',`| Primera generación y escritura | ${range(result.server.coldMs)} |`,`| Lectura de miniatura cacheada | ${range(result.server.cachedMs)} |`,'');
lines.push('Las variantes unified usan recursos idénticos para comunidades y DMs; por eso se mide una sola variante común por formato, evitando duplicar el mismo escenario. Los PNG de DM anterior y unified son idénticos: sus diferencias pequeñas de consumo/carga se tratan como variabilidad. Los originales se verifican byte por byte antes del A/B.','');
fs.writeFileSync(path.join(root,'docs/UNIFIED_IMAGE_MEASUREMENTS.md'),lines.join('\n'));
console.log('Wrote unified measurements and statistics');
