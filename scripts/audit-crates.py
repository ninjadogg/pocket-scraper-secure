import json, sys, time, tomllib, urllib.request
lock = tomllib.load(open(sys.argv[1], 'rb'))
cutoff = sys.argv[2]
rows, bad = [], []
for p in lock['package']:
    if not p.get('source', '').startswith('registry+'):
        continue
    url = f"https://crates.io/api/v1/crates/{p['name']}/{p['version']}"
    req = urllib.request.Request(url, headers={'User-Agent': 'pocket-scraper-secure dependency audit (+https://github.com/ninjadogg/pocket-scraper-secure)'})
    v = json.load(urllib.request.urlopen(req))['version']
    crate = json.load(urllib.request.urlopen(urllib.request.Request(f"https://crates.io/api/v1/crates/{p['name']}", headers=req.headers)))['crate']
    day = v['created_at'][:10]
    row = (p['name'], p['version'], day, v.get('license') or '?', (crate.get('repository') or '?').replace('https://github.com/', ''))
    rows.append(row)
    if day > cutoff or v.get('yanked'):
        bad.append(row)
    time.sleep(1)
for r in rows:
    print('|', ' | '.join(r), '|')
print('\nTOO NEW OR YANKED:', bad or 'none')
