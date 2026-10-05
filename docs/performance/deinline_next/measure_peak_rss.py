"""Isolated Linux child HWM via wait4; separate from timing samples."""
import argparse, json, os, pathlib, subprocess, sys, tempfile

def measure(command):
    with tempfile.TemporaryFile() as log:
        child=subprocess.Popen(command, stdout=subprocess.DEVNULL, stderr=log)
        _, status, usage=os.wait4(child.pid, 0)
        child.returncode=os.waitstatus_to_exitcode(status)
        log.seek(0)
        error=log.read(2000).decode(errors='replace')
    return dict(command=command, exit_code=child.returncode,
                peak_rss_bytes=usage.ru_maxrss*1024,
                user_cpu_seconds=usage.ru_utime, system_cpu_seconds=usage.ru_stime,
                stderr=error[:2000])

def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--report',type=pathlib.Path,required=True)
    parser.add_argument('--control',action='store_true')
    parser.add_argument('command',nargs=argparse.REMAINDER)
    args=parser.parse_args()
    command=args.command[1:] if args.command[:1]==['--'] else args.command
    if args.control:
        code='import sys; n=int(sys.argv[1]); data=bytearray(n); data[::4096]=b"x"*len(data[::4096])'
        rows=[measure([sys.executable,'-I','-c',code,str(n)]) for n in (0,64*1024*1024)]
        growth=rows[1]['peak_rss_bytes']-rows[0]['peak_rss_bytes']
        passed=all(r['exit_code']==0 for r in rows) and 48*1024*1024<=growth<=80*1024*1024
        report=dict(schema_version=1,kind='resident-allocation-control',rows=rows,growth_bytes=growth,passed=passed)
    else:
        if not command:parser.error('missing command')
        row=measure(command)
        report=dict(schema_version=1,kind='single-child-process-memory',row=row,passed=row['exit_code']==0)
    report['contract']='Linux os.wait4 returns per-child ru_maxrss in KiB; multiplied by 1024. Includes process startup, decompiler, output I/O and allocator retention; no heap-allocation-count claim. Measured separately from interleaved timing samples.'
    args.report.parent.mkdir(parents=True,exist_ok=True)
    args.report.write_text(json.dumps(report,indent=2)+'\n')
    print(json.dumps(report))
    return int(not report['passed'])

if __name__=='__main__':raise SystemExit(main())
