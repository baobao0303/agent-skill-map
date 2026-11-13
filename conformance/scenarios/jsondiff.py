"""Compare two ScanResult JSON files modulo volatile fields."""
import json, sys

def scrub(v):
    v.pop("scannedAt", None); v.pop("scannedBy", None)
    v["stats"].pop("durationMs", None)
    for n in v["nodes"]:
        n.pop("modifiedAtMs", None)
        if "bom" in n["path"]:
            n.pop("tokens", None)
    return v

def load(p):
    # The two runs live in sibling workdirs (…/rn-sm vs …/rn-asm).
    text = open(p).read().replace(p.rsplit(".", 2)[0], "<W>")
    return scrub(json.loads(text))

a, b = (load(p) for p in sys.argv[1:3])
if a == b:
    print("SAME"); sys.exit(0)
for k in a:
    if a[k] != b.get(k):
        if isinstance(a[k], list):
            for i, (x, y) in enumerate(zip(a[k], b[k])):
                if x != y:
                    print(k, i, "\n  sm :", json.dumps(x, ensure_ascii=False)[:600], "\n  asm:", json.dumps(y, ensure_ascii=False)[:600]); break
            if len(a[k]) != len(b[k]): print(k, "len", len(a[k]), len(b[k]))
        else:
            print(k, a[k], b.get(k))
sys.exit(1)
