const fs=require('node:fs'),path=require('node:path'),{spawnSync}=require('node:child_process');
const root=path.resolve(__dirname,'..'),out=path.join(root,'target/unified-images');
const cases=['jpeg-medium','jpeg-large','png-alpha'];
const before=path.join(root,'target/dm-server-images'),after=path.join(out,'fixtures');
for(const name of cases){
  for(const file of fs.readdirSync(path.join(after,name,'server'))){
    if(!fs.readFileSync(path.join(after,name,'server',file)).equals(fs.readFileSync(path.join(after,name,'dm',file))))throw Error('Shared policy differs between DM/community');
  }
  for(const file of fs.readdirSync(path.join(before,name,'server'))){
    if(!fs.readFileSync(path.join(before,name,'server',file)).equals(fs.readFileSync(path.join(after,name,'server-original',file))))throw Error('A/B original corpus differs');
  }
}
for(let run=1;run<=5;run++)for(const name of cases){
  const variants=[['community-before','Baseline.exe',path.join(before,name,'server')],['dm-before','Baseline.exe',path.join(before,name,'dm')],['unified','Candidate.exe',path.join(after,name,'server')]];
  if(run%2===0)variants.reverse();
  for(const [kind,exe,fixtures] of variants){
    console.log(new Date().toISOString(),name,kind,run);
    const done=spawnSync(process.execPath,[path.join(root,'scripts/measure-image-memory.cjs'),'--out',out,'--variant',`${name}-${kind}`,'--exe',path.join(out,exe),'--fixtures',fixtures,'--report',path.join(root,'docs/UNIFIED_IMAGE_RESULTS.json'),'--runs',String(run),'--load-cpu'],{cwd:root,stdio:'inherit',windowsHide:true});
    if(done.status!==0)process.exit(done.status||1);
  }
}
