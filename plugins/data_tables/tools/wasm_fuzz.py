"""Damages every committed fixture many times (fixed seed) and runs each damaged file through the
compiled plugin under wasmtime, asserting every outcome is a result or one plain sentence on stderr,
never a trap or a panic. The native `hostile_tests` do the same in-process; this checks the real
WebAssembly build.

    cargo build --release --target wasm32-wasip1
    wasmtime compile target/wasm32-wasip1/release/data_tables.wasm -o /tmp/dt.cwasm
    python3 tools/wasm_fuzz.py 30 /tmp/dt.cwasm
"""
import json,subprocess,os,random,glob,shutil,tempfile,sys
root=os.path.join(os.path.dirname(os.path.abspath(__file__)),'..','tests','fixtures')
files=[f for f in sorted(glob.glob(root+'/formats/*'))+sorted(glob.glob(root+'/hostile/*')) if os.path.getsize(f)<400000]
rng=random.Random(20261005)
def damage(b):
    b=bytearray(b)
    if not b: return bytes([rng.randrange(256)])
    k=rng.randrange(5)
    if k==0:
        for _ in range(1+rng.randrange(8)): b[rng.randrange(len(b))]^=1<<rng.randrange(8)
    elif k==1: del b[rng.randrange(len(b)):]
    elif k==2:
        i=rng.randrange(len(b)); b[i:i]=bytes(rng.randrange(256) for _ in range(1+rng.randrange(16)))
    elif k==3:
        i=rng.randrange(len(b)); n=1+rng.randrange(32); b[i:i+n]=bytes([rng.choice([0,255])])*len(b[i:i+n])
    else:
        i=rng.randrange(len(b)); j=i+1+rng.randrange(64); c=b[i:j]; at=rng.randrange(len(b)); b[at:at]=c
    return bytes(b)
bad=[];runs=0
rounds=int(sys.argv[1]) if len(sys.argv)>1 else 40
cwasm=sys.argv[2] if len(sys.argv)>2 else '/tmp/dt.cwasm'
for f in files:
    name=os.path.basename(f); stem=name.rsplit('.',1)[0]; data=open(f,'rb').read()
    for r in range(rounds):
        d=tempfile.mkdtemp(); open(os.path.join(d,name),'wb').write(damage(data))
        for inp in ({"mode":"tables"},{"sql":"SELECT * FROM %s LIMIT 20"%stem},{"mode":"describe","sample_rows":100}):
            i=dict(inp); i["path"]="/data"
            p=subprocess.run(['wasmtime','run','--allow-precompiled','-W','max-memory-size=3221225472','--dir',d+'::/data',cwasm],input=json.dumps(i),capture_output=True,text=True,timeout=120)
            runs+=1
            ok = p.returncode==0 or (p.returncode==1 and p.stderr.startswith('data_tables: ') and p.stderr.count('\n')==1 and 'trap' not in p.stderr and 'panick' not in p.stderr)
            if not ok: bad.append((name,r,inp,p.returncode,p.stderr[:200]))
        shutil.rmtree(d)
print(runs,'runs',len(bad),'bad')
for b in bad[:20]: print(b)
