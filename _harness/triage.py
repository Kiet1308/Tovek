#!/usr/bin/env python3
# Re-run every program that mismatched/failed at any opt level, normalize
# error-message file paths + line numbers, and print only the TRUE divergences.
import subprocess, re, os, sys, glob

GEN = "gen"
LC = r"D:/Medal/luau-tools/luau-compile.exe"
LUAU = r"D:/Medal/luau-tools/luau.exe"
LIFT = r"D:/Medal/medal-decompiler/target/release/luau-lifter.exe"

def norm(s):
    # collapse "<path>.luau:line:col" and bare "<path>.luau" so runtime error
    # messages that embed the (legitimately different) source file/line match.
    s = re.sub(r'\S*\.luau:\d+(:\d+)?', 'FILE:N', s)
    s = re.sub(r'\S*\.luau', 'FILE', s)
    return s

def run(path):
    try:
        r = subprocess.run([LUAU, path], capture_output=True, text=True, timeout=20)
        return (r.stdout + r.stderr).strip()
    except Exception as e:
        return f"<<timeout/err {e}>>"

# gather mismatched names per opt from results files
names = set()
for opt in (0,1,2):
    rf = f"{GEN}/_results_O{opt}.txt"
    if not os.path.exists(rf): continue
    for line in open(rf, encoding='utf-8', errors='replace'):
        m = re.match(r'(MISMATCH|RUNDEC_FAIL|DECOMP_FAIL)\s+(\S+)', line)
        if m: names.add(m.group(2))

true_mm = []
fp = []
for name in sorted(names):
    src = f"{GEN}/{name}.luau"
    if not os.path.exists(src): continue
    orig = run(src)
    worst = None
    for opt in (0,1,2):
        bc = f"{GEN}/_t.bc"; dec = f"{GEN}/_t.dec.luau"
        c = subprocess.run([LC,"--binary",f"-O{opt}",src], capture_output=True)
        if c.returncode != 0: continue
        open(bc,"wb").write(c.stdout)
        d = subprocess.run([LIFT, bc], capture_output=True, text=True)
        if d.returncode != 0:
            worst = (opt, "<<DECOMPILE FAILED>>", d.stderr.strip()[:200]); break
        open(dec,"w",encoding='utf-8',newline='\n').write(d.stdout)
        decout = run(dec)
        if norm(orig) != norm(decout):
            worst = (opt, orig, decout); break
    if worst is None:
        fp.append(name)
    else:
        true_mm.append((name, worst))

print(f"\n===== TRUE MISMATCHES: {len(true_mm)}  (filtered false positives: {len(fp)}) =====\n")
for name,(opt,orig,decout) in true_mm:
    print(f"### {name}   [O{opt}]")
    print(f"  ORIG: {orig[:400].replace(chr(10),' | ')}")
    print(f"  DEC : {decout[:400].replace(chr(10),' | ')}")
    print()
print("\n===== FALSE POSITIVES (path/line-only diffs): =====")
print(", ".join(fp))
