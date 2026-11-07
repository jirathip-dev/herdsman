import fcntl, json, os, subprocess, sys
t = os.environ["CARGO_TARGET_DIR"]
b = subprocess.run(["cargo", "test", "--locked", "--no-run", "--message-format=json"], capture_output=True, text=True)
if b.returncode:
    print(b.stderr[-3000:]); sys.exit(b.returncode)
exes = []
for l in b.stdout.splitlines():
    m = json.loads(l)
    if m.get("reason") == "compiler-artifact" and m.get("executable") and m["profile"]["test"]:
        exes.append((m["manifest_path"], m["executable"]))
lock = open(os.path.join(t, "debug", ".cargo-lock"), "a")
fcntl.flock(lock, fcntl.LOCK_EX)
fails = 0
for man, exe in exes:
    r = subprocess.run([exe], cwd=os.path.dirname(man), capture_output=True, text=True)
    res = [x for x in r.stdout.splitlines() if x.startswith("test result")]
    print(os.path.basename(exe), r.returncode, res[-1] if res else r.stderr[-500:], flush=True)
    if r.returncode:
        fails += 1
        print(r.stdout[-4000:], r.stderr[-2000:], flush=True)
print("binaries", len(exes), "failed", fails)
sys.exit(1 if fails else 0)
