"""Local browser task fixture with server-side oracles for model integration runs."""

import argparse
import json
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from urllib.parse import parse_qs, urlparse

INVOICES = [
    {"id": "INV-101", "status": "Approved", "due": "2026-10-01", "amount": "149.95"},
    {"id": "INV-102", "status": "Paid", "due": "2026-09-20", "amount": "80.00"},
    {"id": "INV-103", "status": "Approved", "due": "2026-11-01", "amount": "240.00"},
    {"id": "INV-104", "status": "Approved", "due": "2026-10-14", "amount": "320.40"},
    {"id": "INV-105", "status": "Draft", "due": "2026-10-02", "amount": "99.00"},
    {"id": "INV-106", "status": "Approved", "due": "2026-10-15", "amount": "75.25"},
    {"id": "INV-107", "status": "Approved", "due": "2026-09-30", "amount": "29.65"},
    {"id": "INV-108", "status": "Paid", "due": "2026-10-03", "amount": "900.00"},
    {"id": "INV-109", "status": "Approved", "due": "2026-12-01", "amount": "120.00"},
]

PAGE = """<!doctype html><html><head><title>Gents browser practice desk</title>
<style>body{font:18px sans-serif;max-width:900px;margin:40px auto}button,input,select{padding:10px;margin:8px}td,th{padding:12px;border-bottom:1px solid #ddd}</style></head>
<body><h1>Operations desk</h1><nav><a href="/invoices">Invoices</a> | <a href="/request">Delivery request</a> | <a href="/upload">Import CSV</a></nav><main id="app"></main><script>
const app=document.querySelector('#app');
if(location.pathname==='/invoices') {
 let page=1;
 async function load(){app.innerHTML='<p>Loading invoices…</p>';const rows=await (await fetch('/api/invoices?page='+page)).json();setTimeout(()=>{app.innerHTML='<h2>Invoices</h2><p>Page '+page+' of 3. All amounts USD.</p><table><thead><tr><th>Invoice</th><th>Status</th><th>Due date</th><th>Amount</th></tr></thead><tbody>'+rows.map(r=>`<tr><td>${r.id}</td><td>${r.status}</td><td>${r.due}</td><td>${r.amount}</td></tr>`).join('')+'</tbody></table><button id="previous" '+(page===1?'disabled':'')+'>Previous</button><button id="next" '+(page===3?'disabled':'')+'>Next</button>';document.querySelector('#next').onclick=()=>{page++;load()};document.querySelector('#previous').onclick=()=>{page--;load()};},450)}load();
} else if(location.pathname==='/request') {
 app.innerHTML='<h2>Delivery request</h2><form><label>Recipient <input name="recipient" required></label><label>Region <select name="region" required><option value="">Choose</option><option value="west">West</option><option value="east">East</option></select></label><label>Service <select name="service" required disabled><option value="">Choose region first</option></select></label><label>Reference <input name="reference" required></label><label><input name="confirmed" type="checkbox" required>I confirm this request</label><button type="submit">Submit request</button></form><div role="status"></div>';
 const form=document.querySelector('form'); form.region.onchange=()=>{form.service.disabled=true;setTimeout(()=>{form.service.innerHTML='<option value="">Choose service</option><option value="standard">Standard</option><option value="express">Express</option>';form.service.disabled=false;},700)};
 form.onsubmit=async event=>{event.preventDefault();form.querySelector('button').disabled=true;const body=Object.fromEntries(new FormData(form));const result=await(await fetch('/api/requests',{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify(body)})).json();document.querySelector('[role=status]').textContent='Confirmed '+result.confirmation;};
} else if(location.pathname==='/upload') {
 app.innerHTML='<h2>Import CSV</h2><p>Upload a CSV with name,quantity columns. The receipt includes row count and quantity total.</p><input id="csv" type="file" accept=".csv"><button id="import" disabled>Import</button><pre role="status"></pre>';
 const file=document.querySelector('#csv');file.onchange=()=>document.querySelector('#import').disabled=!file.files.length;
 document.querySelector('#import').onclick=async()=>{const body=await file.files[0].text();const result=await(await fetch('/api/import',{method:'POST',body})).json();document.querySelector('[role=status]').textContent=JSON.stringify(result);};
} else {app.innerHTML='<p>Choose a task above.</p>';}
</script></body></html>"""


def make_server(output, port=0):
    lock = threading.Lock()
    state = {"pages": [], "requests": [], "imports": []}

    def save():
        output.write_text(json.dumps(state, indent=2))

    class Handler(BaseHTTPRequestHandler):
        def log_message(self, *args):
            pass

        def send(self, body, mime="application/json"):
            data = body.encode() if isinstance(body, str) else json.dumps(body).encode()
            self.send_response(200)
            self.send_header("Content-Type", mime)
            self.send_header("Content-Length", str(len(data)))
            self.end_headers()
            self.wfile.write(data)

        def do_GET(self):
            url = urlparse(self.path)
            with lock:
                state["pages"].append(self.path)
                save()
            if url.path == "/api/invoices":
                page = int(parse_qs(url.query).get("page", ["1"])[0])
                self.send(INVOICES[(page - 1) * 3 : page * 3])
            else:
                self.send(PAGE, "text/html")

        def do_POST(self):
            raw = self.rfile.read(int(self.headers.get("Content-Length", 0))).decode()
            with lock:
                if self.path == "/api/requests":
                    data = json.loads(raw)
                    state["requests"].append(data)
                    result = {"confirmation": f"SHIP-{len(state['requests']):04d}"}
                elif self.path == "/api/import":
                    import csv
                    import io

                    rows = list(csv.DictReader(io.StringIO(raw)))
                    result = {
                        "rows": len(rows),
                        "quantity_total": sum(int(r["quantity"]) for r in rows),
                    }
                    state["imports"].append({"csv": raw, "receipt": result})
                else:
                    result = {"error": "Unknown route"}
                save()
            self.send(result)

    return ThreadingHTTPServer(("127.0.0.1", port), Handler)


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--port", type=int, default=19472)
    args = parser.parse_args()
    make_server(args.output, args.port).serve_forever()
