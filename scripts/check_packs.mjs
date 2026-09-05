// Manifest/naming/documentation fence. --write-diagrams refreshes the generated
// topology section; normal invocation checks it without modifying files.
import fs from 'node:fs';
import path from 'node:path';
import assert from 'node:assert/strict';
const root = new URL('../packs/', import.meta.url);
const write = process.argv.includes('--write-diagrams');
const read = p => fs.readFileSync(p, 'utf8');
const json = p => JSON.parse(read(p).replace(/\$\{[^}:]+:-([^}]+)\}/g, '$1'));
const catalog = json(new URL('catalog.json', root));
assert.equal(catalog.catalog_version,1);
assert.deepEqual([...catalog.packs].sort(), fs.readdirSync(root,{withFileTypes:true}).filter(e=>e.isDirectory()).map(e=>e.name).sort(), 'register every pack once in catalog.json');
let count = 0;
for (const entry of fs.readdirSync(root, {withFileTypes:true})) {
 if (!entry.isDirectory()) continue;
 const name = entry.name, dir = new URL(name+'/', root);
 assert.match(name, /^[a-z][a-z0-9_]*$/);
 const manifest = json(new URL('manifest.json', dir));
 assert.equal(manifest.name,name);
 assert.equal(manifest.manifest_version,1);
 assert.match(manifest.version,/^\d+\.\d+\.\d+$/);
 assert.ok(manifest.description && manifest.authors.length && manifest.tags.length);
 assert.ok(['graph','documents','assets'].includes(manifest.kind));
 assert.equal(new Set(manifest.assets).size,manifest.assets.length);
 assert.ok(manifest.assets.includes('README.md'));
 for(const asset of manifest.assets) {
   assert.ok(!path.isAbsolute(asset) && !asset.split('/').some(p=>p.startsWith('.') || p==='runs'));
   for(const component of asset.split('/').slice(0,-1))assert.match(component,/^[a-z][a-z0-9_]*$/);
   const filename = asset.split('/').at(-1);
   if(!['README.md','Cargo.toml','Cargo.lock'].includes(filename))assert.match(filename,/^[a-z0-9_]+(\.[a-z0-9_]+)*$/);
   assert.ok(fs.statSync(new URL(asset,dir)).isFile(), `${name}/${asset}`);
 }
 for(const dependency of manifest.dependencies || [])assert.ok(fs.existsSync(new URL(dependency+'/manifest.json',root)));
 const labels = new Map(), edges = [];
 const id = label => {if(!labels.has(label))labels.set(label,'n'+labels.size);return labels.get(label);};
 if(manifest.kind==='graph') {
   const graph=json(new URL(manifest.intent,dir));
   for(const node of graph.nodes)id(node.node_id);
   for(const edge of graph.edges)edges.push(`${id(edge.from.node_id)} -->|${JSON.stringify(edge.from.port+' → '+edge.to.port)}| ${id(edge.to.node_id)}`);
 } else if(fs.existsSync(new URL('event_triggers/',dir))) {
   for(const handle of fs.readdirSync(new URL('event_triggers/',dir))) {
     const trigger=json(new URL('event_triggers/'+handle+'/object.json',dir));
     edges.push(`${id(trigger.source_collection)} -->|${JSON.stringify(trigger.trigger_id)}| ${id(trigger.task_id)}`);
   }
 }
 const readme = new URL('README.md',dir);
 let text=read(readme);
 if(labels.size) {
   const block='<!-- pack-topology:start -->\n```mermaid\nflowchart LR\n'+
     [...labels].map(([label,node])=>`    ${node}[${JSON.stringify(label)}]`).join('\n')+'\n'+
     edges.map(edge=>'    '+edge).join('\n')+'\n```\n<!-- pack-topology:end -->';
   if(write) {
     text=text.includes('<!-- pack-topology:start -->')?text.replace(/<!-- pack-topology:start -->[\s\S]*?<!-- pack-topology:end -->/,block):text+'\n## Declared topology\n\n'+(manifest.kind==='graph'?'Compiled capability edges.':'Document-trigger edges; task writes and host callbacks are described above.')+'\n\n'+block+'\n';
     fs.writeFileSync(readme,text);
   }
   assert.ok(text.includes(block),`${name}: stale/missing generated topology; run node scripts/check_packs.mjs --write-diagrams`);
 }
 assert.ok(text.includes('## '),`${name}: needs documented configuration/usage`);
 count++;
}
console.log(`Validated ${count} pack manifests, assets and topology diagrams.`);
