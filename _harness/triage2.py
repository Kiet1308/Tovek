#!/usr/bin/env python3
# Re-run every program that mismatched/failed at any opt level, normalize
# error-message file paths + line numbers, and print only the TRUE divergences.
import subprocess, re, os, sys, glob, shutil

GEN = "gen2"
REPO_ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), ".."))
WORKSPACE_ROOT = os.path.dirname(REPO_ROOT)

def tool(env_name, *candidates):
    configured = os.environ.get(env_name)
    if configured:
        return configured
    for candidate in candidates:
        if os.path.isfile(candidate):
            return candidate
    return candidates[0]

LC = tool("LUAU_COMPILE_EXE", os.path.join(WORKSPACE_ROOT, "luau-tools", "luau-compile.exe"), os.path.join(REPO_ROOT, "luau-tools", "luau-compile.exe"))
LUAU = tool("LUAU_EXE", os.path.join(WORKSPACE_ROOT, "luau-tools", "luau.exe"), os.path.join(REPO_ROOT, "luau-tools", "luau.exe"))
LIFT = tool("LIFTER_EXE", os.path.join(REPO_ROOT, "target", "release", "luau-lifter.exe"), os.path.join(REPO_ROOT, "target", "debug", "luau-lifter.exe"))

def require_tools():
    missing = [path for path in (LC, LUAU, LIFT)
               if not ((os.path.dirname(path) and os.path.isfile(path)) or shutil.which(path))]
    if missing:
        raise SystemExit("HARNESS_ERROR: set LUAU_COMPILE_EXE, LUAU_EXE, and LIFTER_EXE; missing " + ", ".join(missing))

require_tools()

WSLPATH = shutil.which("wslpath")

def tool_arg(value):
    """Translate existing WSL paths for Windows tools launched via interop."""
    if WSLPATH and isinstance(value, str) and not value.startswith("-") and os.path.exists(value):
        absolute = os.path.abspath(value)
        try:
            return subprocess.check_output([WSLPATH, "-w", absolute], text=True).strip()
        except (OSError, subprocess.SubprocessError):
            pass
    return value

def run_tool(executable, *args, **kwargs):
    return subprocess.run(
        [executable, *(tool_arg(arg) for arg in args)],
        **kwargs,
    )

def norm(s):
    # collapse "<path>.luau:line:col" and bare "<path>.luau" so runtime error
    # messages that embed the (legitimately different) source file/line match.
    s = re.sub(r'\S*\.luau:\d+(:\d+)?', 'FILE:N', s)
    s = re.sub(r'\S*\.luau', 'FILE', s)
    return s

def run(path):
    try:
        r = run_tool(LUAU, path, capture_output=True, text=True, timeout=20)
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
        c = run_tool(LC, "--binary", f"-O{opt}", src, capture_output=True)
        if c.returncode != 0: continue
        open(bc,"wb").write(c.stdout)
        d = run_tool(LIFT, bc, capture_output=True, text=True)
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
